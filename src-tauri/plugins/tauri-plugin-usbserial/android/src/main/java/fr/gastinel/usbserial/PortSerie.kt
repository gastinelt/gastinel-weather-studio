package fr.gastinel.usbserial

import android.hardware.usb.UsbDevice
import android.hardware.usb.UsbDeviceConnection
import com.hoho.android.usbserial.driver.Ch34xSerialDriver
import com.hoho.android.usbserial.driver.UsbSerialPort

/**
 * Liaison série avec le RC-5, par-dessus `usb-serial-for-android`.
 *
 * ## Pourquoi cette classe n'écrit plus le pilote elle-même
 *
 * Le prompt 13 avait écrit à la main la séquence d'initialisation de la puce
 * CH340 (`Ch340.kt`) d'après le pilote de référence Linux `ch341.c`. Essayée sur
 * le matériel — téléphone, adaptateur OTG, RC-5 réel, deux adaptateurs
 * différents — elle **énumérait** l'appareil sans jamais réussir à faire passer
 * un octet. Le détail bas niveau fautif n'a pas été identifié, et il n'y avait
 * aucune raison de le chercher : `mik3y/usb-serial-for-android` fait exactement
 * ce travail depuis 2013, en espace utilisateur, sans root ni pilote noyau, avec
 * un [Ch34xSerialDriver] dédié et exercé sur du matériel réel par ses auteurs
 * (ses tests d'instrumentation parlent à une vraie puce à travers un serveur
 * RFC2217).
 *
 * Ce qui reste ici est donc **uniquement** ce que la bibliothèque ne fait pas et
 * que le protocole RC-5 exige :
 *
 * * le **reliquat de lecture**, voir [reste] ;
 * * la **purge** de l'entrée : `purgeHwBuffers` lève
 *   `UnsupportedOperationException` sur une CH34x — la puce n'expose pas de
 *   commande de vidage — il faut donc drainer en lisant ;
 * * la **traduction des erreurs**, qui conserve le message d'origine de la
 *   bibliothèque et le nomme d'une étape. C'est délibéré : le diagnostic d'un
 *   échec sur le terrain a besoin du texte exact, pas d'une reformulation.
 *
 * ## Ce qu'elle ne fait pas
 *
 * Le protocole RC-5 n'est pas ici — il reste en Rust (`elitech_serial.rs`),
 * validé contre le matériel côté desktop : sommes de contrôle, `0xCC` qui clôt
 * la session, repos de 500 ms entre fermeture et réouverture. Cette classe ne
 * connaît que des octets, un débit et des délais.
 */
class PortSerie(
    device: UsbDevice,
    private val connection: UsbDeviceConnection,
) {
    /**
     * L'appareil dont cette session dépend.
     *
     * Retenu ici et pas seulement dans le paramètre : quand Android annonce un
     * débranchement, il ne donne que l'appareil, et il faut pouvoir retrouver
     * les sessions à fermer (cf. `UsbSerialPlugin.fermerSessionsDe`). Le
     * [UsbDevice] entier n'est pas conservé — il ne survit pas au
     * débranchement, son identifiant si.
     */
    val deviceId: Int = device.deviceId

    /**
     * Le pilote est instancié **directement**, sans passer par
     * `UsbSerialProber.getDefaultProber()`.
     *
     * Le tri a déjà eu lieu : c'est le Rust (`list_ch340_ports`) qui a retenu ce
     * `1a86:7523` et personne d'autre. Sonder à nouveau ne ferait que rendre un
     * refus muet (« aucun pilote pour cet appareil ») là où l'on veut un échec
     * nommé, et empêcherait d'ouvrir un clone de CH340 portant un identifiant
     * que la table de sondage de la bibliothèque ne connaît pas encore.
     */
    private val port: UsbSerialPort = Ch34xSerialDriver(device).ports.first()

    /**
     * Reliquat de lecture.
     *
     * Un transfert en masse rend ce que le contrôleur a bufferisé, pas ce qu'on
     * a demandé. On lit donc toujours dans un tampon d'au moins un paquet
     * entier — demander 3 octets quand la puce en a 64 à donner fait perdre les
     * 61 autres, quand ce n'est pas mettre le transfert en erreur — et ce qui
     * dépasse est **gardé** : le jeter décalerait l'alignement de toutes les
     * trames suivantes, exactement ce que la boucle d'accumulation de
     * `elitech_serial.rs` cherche à éviter.
     */
    private val reste = ArrayDeque<Byte>()

    /**
     * Taille d'un paquet du point de terminaison de lecture, connue seulement
     * après l'ouverture. 64 est la valeur d'une CH340 en pleine vitesse ; elle
     * ne sert que de repli avant [open].
     */
    private var taillePaquet = 64

    /**
     * Ouvre et configure : 115200 8N1, sans contrôle de flux.
     *
     * L'ordre est celui que la bibliothèque attend — `open` réclame les
     * interfaces et initialise la puce (à 9600, son défaut), `setParameters`
     * pose ensuite le vrai débit et le format de trame. Inverser les deux
     * laisserait le port à 9600.
     */
    fun open(baudRate: Int, dtr: Boolean, rts: Boolean) {
        try {
            port.open(connection)
        } catch (e: Exception) {
            fermerSansBruit()
            throw traduire(e, ETAPE_OUVERTURE)
        }
        try {
            port.setParameters(
                baudRate,
                UsbSerialPort.DATABITS_8,
                UsbSerialPort.STOPBITS_1,
                UsbSerialPort.PARITY_NONE,
            )
            // `false` des deux côtés, comme le `dtr_on_open(false)` du desktop :
            // le RC-5 ignore ces lignes, mais la même puce CH340 équipe les
            // clones Arduino, qu'un DTR levé redémarre. La bibliothèque gère
            // elle-même l'inversion des bits dans le registre de la puce.
            port.setDTR(dtr)
            port.setRTS(rts)
            taillePaquet = port.readEndpoint?.maxPacketSize ?: taillePaquet
        } catch (e: Exception) {
            // Sans ça, un échec de configuration laisserait les interfaces
            // réservées : le branchement suivant échouerait sans raison visible.
            fermerSansBruit()
            throw traduire(e, ETAPE_PARAMETRES)
        }
    }

    /** Écrit une trame entière, ou échoue en le disant. */
    fun write(data: ByteArray, timeoutMs: Int) {
        try {
            // La bibliothèque découpe elle-même en paquets et boucle jusqu'au
            // dernier octet ; elle lève `SerialTimeoutException` si le délai
            // expire avant, avec le nombre d'octets réellement partis.
            port.write(data, timeoutMs)
        } catch (e: Exception) {
            throw traduire(e, ETAPE_ECRITURE)
        }
    }

    /**
     * Lit **au plus** [len] octets, dans la limite du délai.
     *
     * Rendre moins que demandé n'est pas une erreur : c'est l'appelant Rust qui
     * connaît la longueur attendue et qui boucle, exactement comme il le fait
     * sur le port série desktop.
     */
    fun read(len: Int, timeoutMs: Int): ByteArray {
        val sortie = ArrayList<Byte>(len)
        while (sortie.size < len && reste.isNotEmpty()) {
            sortie.add(reste.removeFirst())
        }
        if (sortie.size == len) return sortie.toByteArray()

        val debut = System.currentTimeMillis()
        val tampon = ByteArray(maxOf(taillePaquet, len))
        while (sortie.size < len) {
            val restant = timeoutMs - (System.currentTimeMillis() - debut)
            if (restant <= 0) break
            // `read` rend 0 quand le délai s'est écoulé sans rien voir venir, et
            // lève si la liaison est tombée — c'est ce qui distingue « le logger
            // se tait » de « le logger a été débranché ».
            val n = try {
                port.read(tampon, tampon.size, restant.toInt())
            } catch (e: Exception) {
                throw traduire(e, ETAPE_LECTURE)
            }
            if (n <= 0) continue
            for (i in 0 until n) {
                if (sortie.size < len) sortie.add(tampon[i]) else reste.addLast(tampon[i])
            }
        }
        return sortie.toByteArray()
    }

    /**
     * Jette tout ce qui traîne en entrée, reliquat local compris.
     *
     * Pas de `purgeHwBuffers` : la CH34x n'a pas de commande de vidage, et la
     * bibliothèque lève `UnsupportedOperationException` plutôt que de faire
     * semblant. On draine donc en lisant jusqu'au silence, avec un délai court —
     * on ne veut que ce qui est **déjà** arrivé.
     */
    fun purge() {
        reste.clear()
        val tampon = ByteArray(taillePaquet)
        while (true) {
            // Une liaison tombée se dira à la première vraie lecture, avec un
            // message utile ; ici elle n'ajouterait rien.
            val n = runCatching { port.read(tampon, tampon.size, DELAI_PURGE_MS) }.getOrDefault(0)
            if (n <= 0) break
        }
    }

    /** Libère la puce et la connexion. Idempotent. */
    fun close() {
        reste.clear()
        fermerSansBruit()
    }

    /**
     * `UsbSerialPort.close()` ferme aussi la [UsbDeviceConnection], mais **pas**
     * s'il n'a jamais été ouvert : il lève « Already closed » et la connexion
     * resterait ouverte, l'appareil réservé. D'où les deux fermetures, chacune
     * tolérante — les rejouer est sans effet.
     */
    private fun fermerSansBruit() {
        runCatching { port.close() }
        runCatching { connection.close() }
    }

    /**
     * Conserve le message de la bibliothèque **mot pour mot** et le situe.
     *
     * Le prompt 13 a échoué sans qu'on puisse dire où : énumération, ouverture,
     * configuration ou premier échange. Nommer l'étape est ce qui rend le
     * prochain échec diagnosticable depuis un `logcat`, et le message d'origine
     * est ce qui le rend cherchable dans les sources de la bibliothèque.
     */
    private fun traduire(e: Exception, etape: String): UsbSerialException {
        val detail = e.message?.takeIf { it.isNotBlank() } ?: e.javaClass.simpleName
        return UsbSerialException("$etape : $detail", e)
    }

    private companion object {
        const val ETAPE_OUVERTURE = "Ouverture du port série USB (CH34x)"
        const val ETAPE_PARAMETRES = "Configuration série 115200 8N1 (CH34x)"
        const val ETAPE_ECRITURE = "Écriture sur le port série USB"
        const val ETAPE_LECTURE = "Lecture sur le port série USB"

        /**
         * Assez pour laisser un paquet déjà arrivé se rendre, assez peu pour ne
         * pas attendre un paquet qui ne viendra pas.
         */
        const val DELAI_PURGE_MS = 10
    }
}

/** Erreur de la liaison USB, remontée telle quelle au Rust. */
class UsbSerialException(message: String, cause: Throwable? = null) : Exception(message, cause)
