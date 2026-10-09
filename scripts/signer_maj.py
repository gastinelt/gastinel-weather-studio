"""Signature Ed25519 des paquets de mise à jour .gws.

L'application (0.5.0 et suivantes) refuse tout .gws sans `manifest.sig` valide
pour une des clés publiques de `CLES_MAJ` (src-tauri/src/package.rs).
Voir docs/signature-maj.md pour la procédure complète.

La clé privée n'est JAMAIS dans le dépôt : son chemin est passé par `--cle` ou
par la variable d'environnement GWS_CLE_MAJ. Elle est chiffrée par mot de passe
(demandé au clavier, ou GWS_CLE_MAJ_MDP pour un usage non interactif).

Usage :
    python scripts/signer_maj.py generer-cle --sortie <fichier.pem> --key-id maj-2026-a
    python scripts/signer_maj.py signer update-0.5.0.gws [--cle <fichier.pem>] --key-id maj-2026-a
    python scripts/signer_maj.py verifier update-0.5.0.gws [--cle-publique <hex>]

`signer` ajoute (ou remplace) `manifest.sig` dans l'archive et affiche la valeur
à mettre dans le champ `signature` de latest.json / latest-beta.json.
`verifier` contrôle un paquet avec les clés de CLES_MAJ lues dans package.rs
(ou une clé publique donnée), comme le fera l'application.
"""
import argparse
import base64
import getpass
import hashlib
import json
import os
import re
import shutil
import sys
import tempfile
import zipfile
from pathlib import Path

from cryptography.exceptions import InvalidSignature
from cryptography.hazmat.primitives import serialization
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey, Ed25519PublicKey

RACINE = Path(__file__).resolve().parent.parent
PACKAGE_RS = RACINE / "src-tauri" / "src" / "package.rs"
MANIFESTE = "manifest.json"
SIGNATURE = "manifest.sig"
BINAIRES = ("gastinel-weather-studio.exe", "payload/gastinel-weather-studio.exe")
PREFIXE_LATEST = "gws-ed25519-v1"


class Refus(Exception):
    pass


def message_signe(version, min_compat, sha256, taille):
    """Doit rester identique à `message_signe` dans src-tauri/src/package.rs."""
    return f"gws-update-v1\0{version}\0{min_compat or ''}\0{sha256.lower()}\0{taille}".encode()


def mot_de_passe(confirmer=False):
    mdp = os.environ.get("GWS_CLE_MAJ_MDP")
    if mdp:
        return mdp.encode()
    mdp = getpass.getpass("Mot de passe de la clé : ")
    if confirmer and getpass.getpass("Confirmer : ") != mdp:
        raise Refus("les mots de passe diffèrent")
    if len(mdp) < 12:
        raise Refus("mot de passe trop court (12 caractères minimum)")
    return mdp.encode()


def hors_depot(chemin: Path):
    chemin = chemin.resolve()
    if chemin == RACINE or RACINE in chemin.parents:
        raise Refus(f"la clé privée ne doit pas être dans le dépôt : {chemin}")


def chemin_cle(arg):
    brut = arg or os.environ.get("GWS_CLE_MAJ")
    if not brut:
        raise Refus("chemin de la clé privée manquant (--cle ou GWS_CLE_MAJ)")
    p = Path(brut).expanduser()
    hors_depot(p)
    return p


def publique_rust(pub: bytes):
    return "[" + ", ".join(f"0x{b:02x}" for b in pub) + "]"


def generer_cle(sortie: Path, key_id: str):
    hors_depot(sortie)
    if sortie.exists():
        raise Refus(f"le fichier existe déjà, rien n'est écrasé : {sortie}")
    k = Ed25519PrivateKey.generate()
    pem = k.private_bytes(serialization.Encoding.PEM, serialization.PrivateFormat.PKCS8,
                          serialization.BestAvailableEncryption(mot_de_passe(confirmer=True)))
    sortie.parent.mkdir(parents=True, exist_ok=True)
    fd = os.open(sortie, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, "wb") as f:
        f.write(pem)
    pub = k.public_key().public_bytes(serialization.Encoding.Raw, serialization.PublicFormat.Raw)
    print(f"Clé privée chiffrée écrite : {sortie}")
    print("Entrée à ajouter dans CLES_MAJ (src-tauri/src/package.rs) :")
    print(f'    ("{key_id}", {publique_rust(pub)}),')


def charger_privee(p: Path):
    return serialization.load_pem_private_key(p.read_bytes(), password=mot_de_passe())


def lire_paquet(chemin: Path):
    """(manifeste, taille exe, sha256 calculé, manifest.sig brut ou None)."""
    with zipfile.ZipFile(chemin) as z:
        noms = set(z.namelist())
        man = json.loads(z.read(MANIFESTE).decode("utf-8-sig"))
        nom = next((n for n in BINAIRES if n in noms), None)
        if nom is None:
            raise Refus("exécutable absent du paquet")
        exe = z.read(nom)
        sig = z.read(SIGNATURE).decode("utf-8-sig") if SIGNATURE in noms else None
    if man.get("type") != "gws-update":
        raise Refus(f"type de paquet inattendu : {man.get('type')!r}")
    return man, len(exe), hashlib.sha256(exe).hexdigest(), sig


def ecrire_signature(chemin: Path, contenu: str):
    """Réécrit l'archive avec `manifest.sig` (remplacé s'il existait)."""
    fd, tmp = tempfile.mkstemp(suffix=".gws", dir=chemin.parent)
    os.close(fd)
    try:
        with zipfile.ZipFile(chemin) as src, zipfile.ZipFile(tmp, "w", zipfile.ZIP_DEFLATED) as dst:
            for info in src.infolist():
                if info.filename != SIGNATURE:
                    dst.writestr(info, src.read(info.filename))
            dst.writestr(SIGNATURE, contenu)
        shutil.move(tmp, chemin)
    finally:
        if os.path.exists(tmp):
            os.remove(tmp)


def signer(chemin: Path, cle, key_id: str):
    man, taille, sha, _ = lire_paquet(chemin)
    if sha != str(man.get("checksum_sha256", "")).lower():
        raise Refus("le SHA-256 de l'exécutable ne correspond pas au manifeste : paquet non signé")
    msg = message_signe(man["version"], man.get("min_compatible_version"), sha, taille)
    b64 = base64.b64encode(cle.sign(msg)).decode()
    ecrire_signature(chemin, json.dumps({"format": 1, "key_id": key_id, "signature": b64}))
    return f"{PREFIXE_LATEST}:{key_id}:{b64}"


def cles_du_code(texte=None):
    """CLES_MAJ lues dans package.rs : {key_id: octets}."""
    texte = texte if texte is not None else PACKAGE_RS.read_text(encoding="utf-8")
    bloc = re.search(r"pub const CLES_MAJ: &\[\(&str, \[u8; 32\]\)\] = &\[(.*?)\];", texte, re.S)
    if not bloc:
        raise Refus("CLES_MAJ introuvable dans package.rs")
    cles = {}
    for kid, octets in re.findall(r'\(\s*"([^"]+)"\s*,\s*\[([^\]]*)\]\s*\)', bloc.group(1)):
        cles[kid] = bytes(int(x, 0) for x in octets.replace("\n", " ").split(",") if x.strip())
    return cles


def verifier(chemin: Path, cles: dict):
    """Mêmes contrôles que l'application ; lève Refus sinon."""
    man, taille, sha, brut = lire_paquet(chemin)
    if brut is None:
        raise Refus("paquet non signé")
    try:
        s = json.loads(brut)
        sig = base64.b64decode(s["signature"], validate=True)
    except (ValueError, KeyError, TypeError):
        raise Refus("signature illisible")
    if s.get("format") != 1:
        raise Refus("format de signature inconnu")
    if s.get("key_id") not in cles:
        raise Refus(f"clé inconnue : {s.get('key_id')!r}")
    msg = message_signe(man["version"], man.get("min_compatible_version"), man["checksum_sha256"], taille)
    try:
        Ed25519PublicKey.from_public_bytes(cles[s["key_id"]]).verify(sig, msg)
    except (InvalidSignature, ValueError):
        raise Refus("signature invalide")
    if sha != man["checksum_sha256"].lower():
        raise Refus("SHA-256 de l'exécutable différent du manifeste")
    return man["version"], s["key_id"]


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    g = sub.add_parser("generer-cle")
    g.add_argument("--sortie", required=True, type=Path)
    g.add_argument("--key-id", required=True)
    s = sub.add_parser("signer")
    s.add_argument("paquet", type=Path)
    s.add_argument("--cle")
    s.add_argument("--key-id", required=True)
    v = sub.add_parser("verifier")
    v.add_argument("paquet", type=Path)
    v.add_argument("--cle-publique", help="hex (64 caractères) ; défaut : CLES_MAJ de package.rs")
    v.add_argument("--key-id", default="cli")
    a = ap.parse_args(argv)
    try:
        if not re.fullmatch(r"[a-z0-9][a-z0-9._-]{0,40}", getattr(a, "key_id", "x") or ""):
            raise Refus("key_id : minuscules, chiffres, . _ - (41 caractères max)")
        if a.cmd == "generer-cle":
            generer_cle(a.sortie.expanduser(), a.key_id)
        elif a.cmd == "signer":
            cle = charger_privee(chemin_cle(a.cle))
            pub = cle.public_key().public_bytes(serialization.Encoding.Raw, serialization.PublicFormat.Raw)
            if cles_du_code().get(a.key_id) != pub:
                raise Refus(f"la clé « {a.key_id} » de CLES_MAJ (package.rs) ne correspond pas à cette clé "
                            "privée : l'application refuserait le paquet")
            ligne = signer(a.paquet, cle, a.key_id)
            verifier(a.paquet, {a.key_id: pub})  # relecture du paquet produit
            print(f"Paquet signé : {a.paquet}")
            print("Valeur du champ \"signature\" pour latest.json :")
            print(ligne)
        else:
            cles = ({a.key_id: bytes.fromhex(a.cle_publique)} if a.cle_publique else cles_du_code())
            if not cles:
                raise Refus("aucune clé dans CLES_MAJ : l'application refuserait tout paquet")
            version, kid = verifier(a.paquet, cles)
            print(f"OK : {a.paquet} (version {version}, clé {kid})")
    except Refus as e:
        print(f"REFUS : {e}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
