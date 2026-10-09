package fr.gastinel.usbserial

import android.app.Activity
import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.hardware.usb.UsbDevice
import android.hardware.usb.UsbManager
import android.os.Build
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSArray
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.Executors
import java.util.concurrent.atomic.AtomicLong

@InvokeArg
class OuvrirArgs {
    var deviceId: Int = -1
    var baudRate: Int = 115_200
    var dtr: Boolean = false
    var rts: Boolean = false
}

@InvokeArg
class EcrireArgs {
    var sessionId: Long = -1
    lateinit var data: IntArray
    var timeoutMs: Long = 5_000
}

@InvokeArg
class LireArgs {
    var sessionId: Long = -1
    var len: Int = 0
    var timeoutMs: Long = 5_000
}

@InvokeArg
class SessionArgs {
    var sessionId: Long = -1
}

/**
 * Pont entre le Rust et l'API USB Host d'Android.
 *
 * ## Ce qu'il fait, et pas plus
 *
 * Il énumère, demande l'autorisation, ouvre, écrit, lit, ferme. **Le protocole
 * RC-5 n'est pas ici** : il reste en Rust (`elitech_serial.rs`), déjà validé
 * contre le matériel côté desktop. Le dupliquer en Kotlin aurait créé deux
 * implémentations à garder d'accord, dont une seule serait testée.
 *
 * ## Le fil d'exécution
 *
 * Toutes les commandes sauf [listDevices] partent sur un exécuteur à un seul
 * fil. Deux raisons :
 *
 * * `bulkTransfer` est bloquant, et le fil principal d'Android ne doit jamais
 *   l'être — l'application serait déclarée « ne répond pas » ;
 * * **un seul fil**, et non un pool : les échanges avec le logger sont une
 *   conversation ordonnée (écrire, puis lire la réponse). Deux transferts
 *   concurrents sur la même puce mélangeraient les trames.
 *
 * Le Rust, lui, appelle depuis un `spawn_blocking` et attend la réponse : la
 * chaîne est bloquante de bout en bout, comme sur desktop.
 *
 * ## L'autorisation
 *
 * Android exige un accord explicite de l'utilisateur, appareil par appareil, et
 * le donne par une **diffusion asynchrone**. [openDevice] ne peut donc pas
 * répondre tout de suite : il enregistre un récepteur, montre la demande, et ne
 * résout l'appel qu'une fois la réponse connue. Un refus est signalé par le
 * préfixe convenu avec le Rust — c'est le seul cas qu'il doit pouvoir
 * distinguer, puisque c'est le seul qui se rattrape en redemandant.
 */
@TauriPlugin
class UsbSerialPlugin(private val activity: Activity) : Plugin(activity) {

    private val manager by lazy {
        activity.getSystemService(Context.USB_SERVICE) as UsbManager
    }

    /** Voir la note sur le fil d'exécution dans la documentation de la classe. */
    private val executeur = Executors.newSingleThreadExecutor()

    private val sessions = ConcurrentHashMap<Long, PortSerie>()
    private val prochaineSession = AtomicLong(1)

    /**
     * Sessions fermées parce que le câble a été arraché.
     *
     * On retient les identifiants après avoir retiré la session : c'est ce qui
     * permet de répondre « le logger a été débranché » plutôt que « session
     * inconnue » à l'appel Rust qui arrivera juste après. La distinction n'est
     * pas cosmétique — le débranchement se rattrape en rebranchant, la session
     * inconnue est un défaut de programmation, et les deux ne demandent pas la
     * même chose à l'utilisateur.
     *
     * L'ensemble ne grandit pas sans fin : il est vidé au branchement suivant
     * (cf. [openDevice]), le seul moment où ce qu'il contient cesse d'être
     * utile.
     */
    private val debranches = java.util.Collections.newSetFromMap(
        ConcurrentHashMap<Long, Boolean>()
    )

    @Command
    fun listDevices(invoke: Invoke) {
        try {
            val tableau = JSArray()
            for (device in manager.deviceList.values) {
                tableau.put(decrire(device))
            }
            val reponse = JSObject()
            reponse.put("devices", tableau)
            invoke.resolve(reponse)
        } catch (e: Exception) {
            invoke.reject(e.message ?: "Énumération USB impossible.")
        }
    }

    @Command
    fun openDevice(invoke: Invoke) {
        val args = invoke.parseArgs(OuvrirArgs::class.java)
        val device = manager.deviceList.values.firstOrNull { it.deviceId == args.deviceId }
        if (device == null) {
            invoke.reject("Appareil USB ${args.deviceId} introuvable — a-t-il été débranché ?")
            return
        }

        // Une ouverture réussie rend sans objet tout ce qui a été perdu avant :
        // c'est le seul moment où l'ardoise peut être effacée sans risquer de
        // changer un « débranché » en « session inconnue » sous les pieds d'un
        // appel encore en vol.
        debranches.clear()

        if (manager.hasPermission(device)) {
            ouvrirEnFond(invoke, device, args)
            return
        }
        demanderAutorisation(device) { accordee ->
            if (accordee) {
                ouvrirEnFond(invoke, device, args)
            } else {
                invoke.reject("$PREFIXE_REFUS${args.deviceId}")
            }
        }
    }

    @Command
    fun writeBytes(invoke: Invoke) {
        val args = invoke.parseArgs(EcrireArgs::class.java)
        surLeFil(invoke) {
            val port = session(args.sessionId)
            port.write(ByteArray(args.data.size) { args.data[it].toByte() }, args.timeoutMs.toInt())
            JSObject()
        }
    }

    @Command
    fun readBytes(invoke: Invoke) {
        val args = invoke.parseArgs(LireArgs::class.java)
        surLeFil(invoke) {
            val port = session(args.sessionId)
            val lus = port.read(args.len, args.timeoutMs.toInt())
            val tableau = JSArray()
            // `toInt() and 0xff` : sans le masque, un octet ≥ 128 traverserait le
            // pont en entier **négatif** et le Rust le relirait de travers.
            for (b in lus) tableau.put(b.toInt() and 0xff)
            val reponse = JSObject()
            reponse.put("data", tableau)
            reponse
        }
    }

    @Command
    fun purgeInput(invoke: Invoke) {
        val args = invoke.parseArgs(SessionArgs::class.java)
        surLeFil(invoke) {
            session(args.sessionId).purge()
            JSObject()
        }
    }

    @Command
    fun closeDevice(invoke: Invoke) {
        val args = invoke.parseArgs(SessionArgs::class.java)
        surLeFil(invoke) {
            // Fermer une session déjà fermée n'est pas une erreur : le `Drop` du
            // côté Rust peut arriver après une fermeture explicite.
            sessions.remove(args.sessionId)?.close()
            JSObject()
        }
    }


    /**
     * Récepteur du débranchement, armé pour toute la vie du greffon.
     *
     * ## Ce qu'il change
     *
     * Sans lui, arracher le câble ne se voyait qu'au transfert suivant : le
     * `bulkTransfer` rendait -1, et l'utilisateur recevait « Erreur de la
     * liaison USB » au milieu d'une lecture de mémoire de dix secondes — un
     * message qui ne dit ni ce qui s'est passé, ni quoi faire. Pire, la session
     * restait dans la carte, et le rebranchement ouvrait une *seconde* session
     * sur le même appareil, la première gardant l'interface réservée.
     *
     * Sur le terrain, le câble OTG est ce qui lâche en premier : il pend au
     * bout de la tablette et le boîtier est dans la poche. Ce n'est pas un cas
     * limite, c'est le cas ordinaire.
     *
     * ## Pourquoi fermer plutôt que laisser mourir
     *
     * Fermer tout de suite rend l'interface au système, ce qui est la condition
     * pour que le rebranchement — quelques secondes plus tard, une fois le
     * câble remis — réussisse du premier coup. Une session laissée ouverte sur
     * un appareil disparu reste réservée jusqu'à la fin du processus.
     */
    private val recepteurDebranchement = object : BroadcastReceiver() {
        override fun onReceive(context: Context, intent: Intent) {
            if (intent.action != UsbManager.ACTION_USB_DEVICE_DETACHED) return
            val device = intent.parcelableExtra<UsbDevice>(UsbManager.EXTRA_DEVICE) ?: return
            fermerSessionsDe(device.deviceId)
        }
    }

    /**
     * Ferme toutes les sessions ouvertes sur un appareil donné.
     *
     * Le ménage part sur l'exécuteur et non sur le fil de la diffusion : une
     * fermeture attend la fin du transfert en cours (`bulkTransfer` est
     * bloquant), et tenir le fil principal pendant ce temps ferait apparaître
     * « l'application ne répond pas » au moment précis où l'utilisateur vient
     * de débrancher.
     *
     * Les appels Rust déjà en vol ne sont pas interrompus : ils échoueront
     * d'eux-mêmes sur une session disparue, avec le message de [session] — qui
     * nomme la cause au lieu de la taire.
     */
    private fun fermerSessionsDe(deviceId: Int) {
        val perdues = sessions.filterValues { it.deviceId == deviceId }.keys
        if (perdues.isEmpty()) return
        executeur.execute {
            for (id in perdues) {
                runCatching { sessions.remove(id)?.close() }
            }
        }
        debranches.addAll(perdues)
    }

    /**
     * `getParcelableExtra` sans surcharge dépréciée.
     *
     * La forme à deux arguments n'existe qu'à partir d'Android 13 ; l'ancienne
     * y est dépréciée mais reste la seule disponible en dessous, d'où les deux
     * branches plutôt qu'une suppression d'avertissement.
     */
    private inline fun <reified T> Intent.parcelableExtra(nom: String): T? =
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            getParcelableExtra(nom, T::class.java)
        } else {
            @Suppress("DEPRECATION")
            getParcelableExtra(nom) as? T
        }

    override fun load(webView: android.webkit.WebView) {
        super.load(webView)
        val filtre = IntentFilter(UsbManager.ACTION_USB_DEVICE_DETACHED)
        // Diffusion du système, jamais d'une autre application : `NOT_EXPORTED`,
        // comme pour l'autorisation (cf. `demanderAutorisation`).
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            activity.registerReceiver(recepteurDebranchement, filtre, Context.RECEIVER_NOT_EXPORTED)
        } else {
            @Suppress("UnspecifiedRegisterReceiverFlag")
            activity.registerReceiver(recepteurDebranchement, filtre)
        }
    }

    private fun ouvrirEnFond(invoke: Invoke, device: UsbDevice, args: OuvrirArgs) {
        surLeFil(invoke) {
            val connection = manager.openDevice(device)
                ?: throw UsbSerialException(
                    "Ouverture refusée par le système pour l'appareil ${args.deviceId}."
                )
            // La connexion est **à nous** dès cette ligne : si la construction du
            // port échoue, personne d'autre ne la fermera, et l'appareil
            // resterait réservé jusqu'au débranchement. [PortSerie.open] se
            // charge du même ménage pour ses propres échecs.
            val port = try {
                PortSerie(device, connection)
            } catch (e: Exception) {
                runCatching { connection.close() }
                throw e
            }
            port.open(args.baudRate, args.dtr, args.rts)
            val id = prochaineSession.getAndIncrement()
            sessions[id] = port
            val reponse = JSObject()
            reponse.put("sessionId", id)
            reponse
        }
    }

    private fun session(id: Long): PortSerie =
        sessions[id] ?: throw UsbSerialException(
            if (debranches.contains(id)) "$PREFIXE_DEBRANCHE$id"
            else "Session USB $id fermée ou inconnue."
        )

    private fun surLeFil(invoke: Invoke, bloc: () -> JSObject) {
        executeur.execute {
            try {
                invoke.resolve(bloc())
            } catch (e: Exception) {
                invoke.reject(e.message ?: "Erreur de la liaison USB.")
            }
        }
    }

    private fun decrire(device: UsbDevice): JSObject {
        val o = JSObject()
        o.put("device_id", device.deviceId)
        o.put("vendor_id", device.vendorId)
        o.put("product_id", device.productId)
        o.put("device_name", device.deviceName)
        o.put("product_name", device.productName)
        o.put("manufacturer_name", device.manufacturerName)
        // `serialNumber` demande l'autorisation sur les versions récentes ; le
        // lire sans l'avoir lève une exception. Un numéro absent est une
        // information acceptable, un plantage à l'énumération ne l'est pas.
        o.put("serial_number", runCatching { device.serialNumber }.getOrNull())
        o.put("has_permission", manager.hasPermission(device))
        return o
    }

    private fun demanderAutorisation(device: UsbDevice, suite: (Boolean) -> Unit) {
        val recepteur = object : BroadcastReceiver() {
            override fun onReceive(context: Context, intent: Intent) {
                if (intent.action != ACTION_AUTORISATION) return
                runCatching { activity.unregisterReceiver(this) }
                val accordee = intent.getBooleanExtra(UsbManager.EXTRA_PERMISSION_GRANTED, false)
                suite(accordee)
            }
        }

        val filtre = IntentFilter(ACTION_AUTORISATION)
        // À partir d'Android 13, enregistrer un récepteur sans dire s'il est
        // exporté est une erreur fatale. La diffusion vient du système et nous
        // est adressée : `NOT_EXPORTED` est le bon niveau.
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            activity.registerReceiver(recepteur, filtre, Context.RECEIVER_NOT_EXPORTED)
        } else {
            @Suppress("UnspecifiedRegisterReceiverFlag")
            activity.registerReceiver(recepteur, filtre)
        }

        // `FLAG_MUTABLE` : le système renseigne lui-même l'appareil et le verdict
        // dans l'intention avant de la renvoyer. Un `PendingIntent` immuable la
        // ferait revenir vide, et toute demande paraîtrait refusée.
        val drapeaux = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_MUTABLE
        } else {
            PendingIntent.FLAG_UPDATE_CURRENT
        }
        val intention = PendingIntent.getBroadcast(
            activity,
            0,
            Intent(ACTION_AUTORISATION).setPackage(activity.packageName),
            drapeaux,
        )
        manager.requestPermission(device, intention)
    }

    companion object {
        /**
         * Action de la diffusion d'autorisation. Propre à l'application —
         * `setPackage` ci-dessus la garde interne.
         */
        private const val ACTION_AUTORISATION = "fr.gastinel.usbserial.AUTORISATION_USB"

        /**
         * Marqueur de refus, reconnu par `lib.rs`.
         * **Les deux côtés doivent rester d'accord sur cette chaîne.**
         */
        private const val PREFIXE_REFUS = "PERMISSION_DENIED:"

        /**
         * Marqueur de débranchement à chaud, reconnu par `lib.rs`.
         * **Les deux côtés doivent rester d'accord sur cette chaîne.**
         */
        private const val PREFIXE_DEBRANCHE = "DEVICE_DETACHED:"
    }
}
