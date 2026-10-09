"""Tests de scripts/signer_maj.py : python -m unittest scripts/test_signer_maj.py

Toutes les clés sont des clés de TEST générées dans un dossier temporaire
(hors du dépôt) et détruites à la fin.
"""
import base64
import hashlib
import json
import os
import re
import sys
import tempfile
import unittest
import zipfile
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))
import signer_maj as sm  # noqa: E402
from cryptography.hazmat.primitives import serialization  # noqa: E402
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey  # noqa: E402

EXE = b"MZ_DUMMY_EXE_CONTENT"


def paquet(dossier, exe=EXE, version="99.0.0", sha=None):
    p = Path(dossier) / f"update-{version}.gws"
    man = {"type": "gws-update", "version": version, "min_compatible_version": "0.1.0",
           "checksum_sha256": sha or hashlib.sha256(exe).hexdigest()}
    with zipfile.ZipFile(p, "w") as z:
        z.writestr("manifest.json", "﻿" + json.dumps(man))
        z.writestr("gastinel-weather-studio.exe", exe)
    return p


def remplacer(p, nom, contenu):
    with zipfile.ZipFile(p) as z:
        entrees = {n: z.read(n) for n in z.namelist()}
    entrees[nom] = contenu
    with zipfile.ZipFile(p, "w") as z:
        for n, c in entrees.items():
            z.writestr(n, c)


class Tests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = self.tmp.name
        self.cle = Ed25519PrivateKey.generate()
        self.pub = self.cle.public_key().public_bytes(serialization.Encoding.Raw, serialization.PublicFormat.Raw)
        self.cles = {"test-a": self.pub}

    def tearDown(self):
        self.tmp.cleanup()

    def test_signature_valide(self):
        p = paquet(self.dir)
        ligne = sm.signer(p, self.cle, "test-a")
        self.assertTrue(ligne.startswith("gws-ed25519-v1:test-a:"))
        self.assertEqual(sm.verifier(p, self.cles), ("99.0.0", "test-a"))
        # Signer deux fois remplace manifest.sig au lieu de le dupliquer.
        sm.signer(p, self.cle, "test-a")
        with zipfile.ZipFile(p) as z:
            self.assertEqual(z.namelist().count("manifest.sig"), 1)

    def test_signature_absente(self):
        with self.assertRaisesRegex(sm.Refus, "non signé"):
            sm.verifier(paquet(self.dir), self.cles)

    def test_signature_invalide(self):
        p = paquet(self.dir)
        sm.signer(p, Ed25519PrivateKey.generate(), "test-a")
        with self.assertRaisesRegex(sm.Refus, "signature invalide"):
            sm.verifier(p, self.cles)
        remplacer(p, "manifest.sig", "placeholder_sig")
        with self.assertRaisesRegex(sm.Refus, "illisible"):
            sm.verifier(p, self.cles)
        sm.signer(p, self.cle, "autre")
        with self.assertRaisesRegex(sm.Refus, "clé inconnue"):
            sm.verifier(p, self.cles)

    def test_paquet_altere(self):
        p = paquet(self.dir)
        sm.signer(p, self.cle, "test-a")
        forge = b"MZ_EXE_MALVEILLANT"
        remplacer(p, "gastinel-weather-studio.exe", forge)
        remplacer(p, "manifest.json", json.dumps({"type": "gws-update", "version": "99.0.0",
                                                   "min_compatible_version": "0.1.0",
                                                   "checksum_sha256": hashlib.sha256(forge).hexdigest()}))
        with self.assertRaisesRegex(sm.Refus, "signature invalide"):
            sm.verifier(p, self.cles)

    def test_refus_de_signer_un_paquet_incoherent(self):
        with self.assertRaisesRegex(sm.Refus, "SHA-256"):
            sm.signer(paquet(self.dir, sha="00" * 32), self.cle, "test-a")

    def test_cle_privee_chiffree_hors_depot(self):
        with mock.patch.dict(os.environ, {"GWS_CLE_MAJ_MDP": "mot-de-passe-de-test"}):
            sortie = Path(self.dir) / "cle_test.pem"
            sm.generer_cle(sortie, "test-a")
            self.assertIn(b"ENCRYPTED PRIVATE KEY", sortie.read_bytes())
            self.assertIsNotNone(sm.charger_privee(sortie))
            with self.assertRaisesRegex(sm.Refus, "existe déjà"):
                sm.generer_cle(sortie, "test-a")
            with self.assertRaisesRegex(sm.Refus, "dans le dépôt"):
                sm.generer_cle(sm.RACINE / "cle.pem", "test-a")

    def test_vecteur_partage_avec_rust(self):
        """Même message et même signature que `vecteur_partage_avec_le_script` (package.rs)."""
        rs = sm.PACKAGE_RS.read_text(encoding="utf-8")
        attendu = re.search(r'const VECTEUR_SIGNATURE: &str = "([^"]+)"', rs).group(1)
        k = Ed25519PrivateKey.from_private_bytes(bytes([7] * 32))
        msg = sm.message_signe("99.0.0", "0.1.0", hashlib.sha256(EXE).hexdigest(), len(EXE))
        self.assertEqual(base64.b64encode(k.sign(msg)).decode(), attendu)

    def test_cles_du_code_lisibles(self):
        self.assertIsInstance(sm.cles_du_code(), dict)
        rs = ('pub const CLES_MAJ: &[(&str, [u8; 32])] = &[\n    ("maj-a", ' + sm.publique_rust(self.pub)
              + '),\n    ("maj-b", ' + sm.publique_rust(bytes(32)) + '),\n];')
        self.assertEqual(sm.cles_du_code(rs), {"maj-a": self.pub, "maj-b": bytes(32)})


if __name__ == "__main__":
    unittest.main()
