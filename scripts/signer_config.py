"""Signature Ed25519 de la configuration du relai SSH (config.json -> config.sig).

L'application lit `~/gws-relais/config.json` et `~/gws-relais/config.sig` sur le
serveur relai et n'accepte la configuration que si la signature est valide pour
une des clés de `CLES_MAJ` (src-tauri/src/package.rs) : ce sont les MÊMES clés
que les mises à jour, mais avec un message préfixé par un contexte distinct,
si bien qu'une signature de mise à jour n'est jamais une signature de
configuration (et inversement). Voir docs/config-relais.md.

Message signé : b"GWS-CONFIG-RELAIS-V1\\0" + octets EXACTS de config.json.
Ne modifiez plus config.json après la signature (même une fin de ligne) :
l'application vérifie les octets tels quels.

La clé privée n'est JAMAIS dans le dépôt : son chemin est passé par `--cle` ou
par la variable d'environnement GWS_CLE_MAJ. Le mot de passe est demandé au
clavier (saisie masquée), ou lu dans GWS_CLE_MAJ_MDP pour un usage non
interactif (même convention que signer_maj.py) ; jamais en argument.

Usage :
    python scripts/signer_config.py signer config.json [--cle <fichier.pem>] --key-id maj-2026-a
    python scripts/signer_config.py verifier config.json [--sig config.sig] [--cle-publique <hex>]

`signer` écrit `config.sig` à côté de `config.json` (ou `--sortie`).
`verifier` refait les contrôles de l'application (signature, format, taille,
expiration ; pas l'anti-rejeu, qui dépend de ce que le poste a déjà accepté).
"""
import argparse
import base64
import datetime as dt
import json
import re
import sys
from pathlib import Path

from cryptography.exceptions import InvalidSignature
from cryptography.hazmat.primitives import serialization
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey

sys.path.insert(0, str(Path(__file__).resolve().parent))
import signer_maj as sm  # noqa: E402  (mêmes clés, même gestion du mot de passe)

Refus = sm.Refus
CONTEXTE = b"GWS-CONFIG-RELAIS-V1\0"
TAILLE_MAX = 64 * 1024


def message_signe(config: bytes) -> bytes:
    """Doit rester identique à `message_signe_config` dans src-tauri/src/relais_config.rs."""
    return CONTEXTE + config


def expiration(texte):
    """Instant d'expiration (UTC) : RFC 3339, ou AAAA-MM-JJ valable jusqu'à la fin du jour."""
    t = str(texte).strip()
    if re.fullmatch(r"\d{4}-\d{2}-\d{2}", t):
        jour = dt.date.fromisoformat(t)
        return dt.datetime.combine(jour + dt.timedelta(days=1), dt.time(), dt.timezone.utc)
    try:
        d = dt.datetime.fromisoformat(t.replace("Z", "+00:00"))
    except ValueError:
        raise Refus(f"date d'expiration illisible : {t!r}")
    if d.tzinfo is None:
        raise Refus(f"date d'expiration sans fuseau : {t!r}")
    return d.astimezone(dt.timezone.utc)


def controler(config: bytes, maintenant=None):
    """Contrôles de contenu de l'application (hors signature et anti-rejeu)."""
    if len(config) > TAILLE_MAX:
        raise Refus(f"config.json trop volumineux (plus de {TAILLE_MAX} octets)")
    try:
        c = json.loads(config.decode("utf-8-sig"))
    except (UnicodeDecodeError, ValueError) as e:
        raise Refus(f"config.json illisible : {e}")
    if not isinstance(c, dict):
        raise Refus("config.json doit être un objet JSON")
    if type(c.get("format")) is not int or c["format"] != 1:
        raise Refus(f"format de configuration inconnu : {c.get('format')!r} (attendu 1)")
    serial = c.get("serial")
    if not isinstance(serial, int) or isinstance(serial, bool) or serial < 0:
        raise Refus("serial : entier >= 0 attendu")
    for champ in ("emis_le", "serveur"):
        if not isinstance(c.get(champ), str):
            raise Refus(f"champ {champ!r} manquant ou non textuel")
    if c.get("expire_le") is not None:
        fin = expiration(c["expire_le"])
        if (maintenant or dt.datetime.now(dt.timezone.utc)) >= fin:
            raise Refus(f"configuration expirée (expire_le = {c['expire_le']})")
    x = c.get("extension")
    if x is not None:
        if not isinstance(x, dict) or not all(isinstance(x.get(k), str) for k in ("nom", "version", "url", "sha256")):
            raise Refus("extension : objet {nom, version, url, sha256} attendu")
        if not x["nom"].strip() or not x["version"].strip():
            raise Refus("extension : nom ou version vide")
        if not x["url"].startswith("https://"):
            raise Refus("extension : l'adresse doit commencer par https://")
        if not re.fullmatch(r"[0-9a-fA-F]{64}", x["sha256"]):
            raise Refus("extension : sha256 attendu en 64 caractères hexadécimaux")
    q = c.get("quota")
    if q is not None and not isinstance(q, dict):
        raise Refus("quota : objet attendu")
    return c


def signer(chemin: Path, cle, key_id: str, sortie: Path = None, maintenant=None):
    """Écrit config.sig pour les octets exacts de `chemin` ; renvoie son chemin."""
    config = chemin.read_bytes()
    controler(config, maintenant)
    b64 = base64.b64encode(cle.sign(message_signe(config))).decode()
    sortie = sortie or chemin.with_name("config.sig")
    # Pas de saut de ligne final : le fichier est du JSON compact, identique partout.
    sortie.write_bytes(json.dumps({"format": 1, "key_id": key_id, "signature": b64}).encode())
    return sortie


def verifier(config: bytes, sig_brute: bytes, cles: dict, maintenant=None):
    """Mêmes contrôles que l'application (sauf l'anti-rejeu) ; lève Refus sinon."""
    if len(sig_brute) > TAILLE_MAX:
        raise Refus("config.sig trop volumineux")
    try:
        s = json.loads(sig_brute.decode("utf-8-sig"))
        sig = base64.b64decode(s["signature"], validate=True)
    except (UnicodeDecodeError, ValueError, KeyError, TypeError):
        raise Refus("signature illisible")
    if s.get("format") != 1:
        raise Refus("format de signature inconnu")
    if s.get("key_id") not in cles:
        raise Refus(f"clé inconnue : {s.get('key_id')!r}")
    try:
        Ed25519PublicKey.from_public_bytes(cles[s["key_id"]]).verify(sig, message_signe(config))
    except (InvalidSignature, ValueError):
        raise Refus("signature invalide")
    c = controler(config, maintenant)
    return c["serial"], s["key_id"]


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    s = sub.add_parser("signer")
    s.add_argument("config", type=Path)
    s.add_argument("--cle")
    s.add_argument("--key-id", required=True)
    s.add_argument("--sortie", type=Path, help="défaut : config.sig à côté de config.json")
    v = sub.add_parser("verifier")
    v.add_argument("config", type=Path)
    v.add_argument("--sig", type=Path, help="défaut : config.sig à côté de config.json")
    v.add_argument("--cle-publique", help="hex (64 caractères) ; défaut : CLES_MAJ de package.rs")
    v.add_argument("--key-id", default="cli")
    a = ap.parse_args(argv)
    try:
        if not re.fullmatch(r"[a-z0-9][a-z0-9._-]{0,40}", a.key_id or ""):
            raise Refus("key_id : minuscules, chiffres, . _ - (41 caractères max)")
        if a.cmd == "signer":
            cle = sm.charger_privee(sm.chemin_cle(a.cle))
            pub = cle.public_key().public_bytes(serialization.Encoding.Raw, serialization.PublicFormat.Raw)
            if sm.cles_du_code().get(a.key_id) != pub:
                raise Refus(f"la clé « {a.key_id} » de CLES_MAJ (package.rs) ne correspond pas à cette clé "
                            "privée : l'application refuserait la configuration")
            sortie = signer(a.config, cle, a.key_id, a.sortie)
            serial, _ = verifier(a.config.read_bytes(), sortie.read_bytes(), {a.key_id: pub})
            print(f"Signature écrite : {sortie} (serial {serial}, clé {a.key_id})")
            print("Copiez config.json et config.sig, sans les modifier, dans ~/gws-relais/ sur le serveur.")
        else:
            cles = ({a.key_id: bytes.fromhex(a.cle_publique)} if a.cle_publique else sm.cles_du_code())
            if not cles:
                raise Refus("aucune clé dans CLES_MAJ : l'application refuserait toute configuration")
            sig = a.sig or a.config.with_name("config.sig")
            serial, kid = verifier(a.config.read_bytes(), sig.read_bytes(), cles)
            print(f"OK : {a.config} (serial {serial}, clé {kid})")
    except Refus as e:
        print(f"REFUS : {e}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
