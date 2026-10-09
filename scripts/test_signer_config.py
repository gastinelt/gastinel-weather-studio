"""Tests de scripts/signer_config.py : python -m unittest scripts/test_signer_config.py

Toutes les clés sont des clés de TEST : générées à la volée, ou dérivées de la
graine publique [7] * 32 pour le vecteur partagé avec le Rust. Aucune vraie clé.
"""
import base64
import datetime as dt
import json
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import signer_config as sc  # noqa: E402
import signer_maj as sm  # noqa: E402
from cryptography.hazmat.primitives import serialization  # noqa: E402
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey  # noqa: E402

VECTEUR = sm.RACINE / "src-tauri" / "src" / "testdata" / "config-relais"
MAINTENANT = dt.datetime(2026, 9, 29, 12, tzinfo=dt.timezone.utc)


def pub(k):
    return k.public_key().public_bytes(serialization.Encoding.Raw, serialization.PublicFormat.Raw)


class Tests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = Path(self.tmp.name)
        self.cle = Ed25519PrivateKey.generate()
        self.cles = {"test-a": pub(self.cle)}

    def tearDown(self):
        self.tmp.cleanup()

    def config(self, **champs):
        c = {"format": 1, "serial": 1, "emis_le": "2026-09-29", "serveur": "relais-test"}
        c.update(champs)
        p = self.dir / "config.json"
        p.write_bytes(json.dumps(c, indent=2).encode())
        return p

    def test_signature_valide(self):
        p = self.config(quota={"a": 1}, inconnu=True)
        sig = sc.signer(p, self.cle, "test-a", maintenant=MAINTENANT)
        self.assertEqual(sig.name, "config.sig")
        self.assertEqual(sc.verifier(p.read_bytes(), sig.read_bytes(), self.cles, MAINTENANT), (1, "test-a"))

    def test_octets_exacts(self):
        p = self.config()
        sig = sc.signer(p, self.cle, "test-a", maintenant=MAINTENANT).read_bytes()
        # JSON équivalent mais octets différents (fins de ligne Windows) : refusé.
        crlf = p.read_bytes().replace(b"\n", b"\r\n")
        with self.assertRaisesRegex(sc.Refus, "signature invalide"):
            sc.verifier(crlf, sig, self.cles, MAINTENANT)

    def test_mauvaise_cle_et_cle_inconnue(self):
        p = self.config()
        sig = sc.signer(p, Ed25519PrivateKey.generate(), "test-a", maintenant=MAINTENANT).read_bytes()
        with self.assertRaisesRegex(sc.Refus, "signature invalide"):
            sc.verifier(p.read_bytes(), sig, self.cles, MAINTENANT)
        sig = sc.signer(p, self.cle, "autre", maintenant=MAINTENANT).read_bytes()
        with self.assertRaisesRegex(sc.Refus, "clé inconnue"):
            sc.verifier(p.read_bytes(), sig, self.cles, MAINTENANT)
        with self.assertRaisesRegex(sc.Refus, "illisible"):
            sc.verifier(p.read_bytes(), b"placeholder", self.cles, MAINTENANT)

    def test_contexte_distinct_des_mises_a_jour(self):
        """Une signature de mise à jour n'est pas une signature de configuration."""
        msg_maj = sm.message_signe("99.0.0", "0.1.0", "ab" * 32, 20)
        self.assertNotEqual(msg_maj[:1], sc.CONTEXTE[:1])
        sig_maj = json.dumps({"format": 1, "key_id": "test-a",
                              "signature": base64.b64encode(self.cle.sign(msg_maj)).decode()}).encode()
        with self.assertRaisesRegex(sc.Refus, "signature invalide"):
            sc.verifier(msg_maj, sig_maj, self.cles, MAINTENANT)
        # Et la signature sur les octets bruts (sans contexte) non plus.
        p = self.config()
        brute = json.dumps({"format": 1, "key_id": "test-a",
                            "signature": base64.b64encode(self.cle.sign(p.read_bytes())).decode()}).encode()
        with self.assertRaisesRegex(sc.Refus, "signature invalide"):
            sc.verifier(p.read_bytes(), brute, self.cles, MAINTENANT)

    def test_refus_de_signer_un_contenu_invalide(self):
        cas = [
            ({"format": 2}, "format"),
            ({"format": 1.0}, "format"),
            ({"serial": -1}, "serial"),
            ({"serial": "3"}, "serial"),
            ({"serveur": None}, "serveur"),
            ({"expire_le": "2026-09-28"}, "expirée"),
            ({"expire_le": "demain"}, "illisible"),
            ({"extension": {"nom": "a", "version": "1", "url": "http://x", "sha256": "0" * 64}}, "https"),
            ({"extension": {"nom": "a", "version": "1", "url": "https://x", "sha256": "0"}}, "sha256"),
            ({"quota": [1]}, "quota"),
        ]
        for champs, motif in cas:
            with self.subTest(champs=champs), self.assertRaisesRegex(sc.Refus, motif):
                sc.signer(self.config(**champs), self.cle, "test-a", maintenant=MAINTENANT)
        p = self.dir / "config.json"
        p.write_bytes(b" " * (sc.TAILLE_MAX + 1))
        with self.assertRaisesRegex(sc.Refus, "volumineux"):
            sc.signer(p, self.cle, "test-a")

    def test_expiration_jour_inclus(self):
        p = self.config(expire_le="2026-09-29")
        sig = sc.signer(p, self.cle, "test-a", maintenant=MAINTENANT).read_bytes()
        self.assertEqual(sc.verifier(p.read_bytes(), sig, self.cles, MAINTENANT)[0], 1)
        demain = MAINTENANT + dt.timedelta(days=1)
        with self.assertRaisesRegex(sc.Refus, "expirée"):
            sc.verifier(p.read_bytes(), sig, self.cles, demain)

    def test_vecteur_partage_avec_rust(self):
        """Le vecteur commité (lu par `vecteur_signe_par_le_script_python`, relais_config.rs)
        est exactement ce que produit ce script avec la clé de test de graine [7] * 32."""
        k = Ed25519PrivateKey.from_private_bytes(bytes([7] * 32))
        copie = self.dir / "config.json"
        copie.write_bytes((VECTEUR / "config.json").read_bytes())
        sig = sc.signer(copie, k, "test-a", maintenant=MAINTENANT).read_bytes()
        self.assertEqual(sig, (VECTEUR / "config.sig").read_bytes())
        self.assertEqual(sc.verifier(copie.read_bytes(), sig, {"test-a": pub(k)}, MAINTENANT), (1, "test-a"))
        # Le Rust utilise le même contexte.
        rs = (sm.RACINE / "src-tauri" / "src" / "relais_config.rs").read_text(encoding="utf-8")
        self.assertIn('pub const CONTEXTE: &[u8] = b"GWS-CONFIG-RELAIS-V1\\0";', rs)

    def test_cli_verifier_avec_cle_publique(self):
        p = self.config()
        sc.signer(p, self.cle, "test-a", maintenant=MAINTENANT)
        self.assertEqual(sc.main(["verifier", str(p), "--cle-publique", pub(self.cle).hex(), "--key-id", "test-a"]), 0)
        self.assertEqual(sc.main(["verifier", str(p), "--cle-publique", pub(Ed25519PrivateKey.generate()).hex(),
                                  "--key-id", "test-a"]), 1)


if __name__ == "__main__":
    unittest.main()
