"""Publication des extensions GWS sur gastinel.me.

Les extensions ne sont pas livrées avec l'application : elle les télécharge
depuis https://gastinel.me/gws/downloads/extensions/ (catalogue.json + une
archive .zip par version) et refuse toute archive dont la signature Ed25519 ne
correspond pas à la clé publique intégrée (`CLE_PUBLIQUE` dans
src-tauri/src/extensions_catalogue.rs).

La clé privée reste sur ce poste, hors du dépôt :
    %USERPROFILE%\\.gws\\cle_extensions_ed25519.pem

Usage :
    python publier_extension.py generer-cle          # une seule fois
    python publier_extension.py paquet ../../extensions/carte3d
    python publier_extension.py envoyer               # dépose dist/ sur le serveur

`paquet` compile en release, crée l'archive, la signe et met à jour
dist/catalogue.json (repris du serveur s'il existe). `envoyer` ne fait que
copier dist/ vers /srv/gws-updates/extensions/ sur le VPS (scp).
"""
import base64
import hashlib
import json
import os
import subprocess
import sys
import urllib.error
import urllib.request
import zipfile
from pathlib import Path

from cryptography.hazmat.primitives import serialization
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

CLE = Path(os.environ["USERPROFILE"]) / ".gws" / "cle_extensions_ed25519.pem"
ICI = Path(__file__).resolve().parent
DIST = ICI / "dist"
URL_BASE = "https://gastinel.me/gws/downloads/extensions/"
HOTE = "cornafsuisse"  # alias SSH (~/.ssh/config), pas d'adresse ici
DEST = "/srv/gws-updates/extensions"


def message_signe(id_, version, api, sha256):
    """Doit rester identique à `message_signe` dans extensions_catalogue.rs."""
    return f"gws-extension-v1\0{id_}\0{version}\0{api}\0{sha256}".encode()


def generer_cle():
    if CLE.exists():
        sys.exit(f"La clé existe déjà : {CLE} (rien n'est écrasé).")
    CLE.parent.mkdir(parents=True, exist_ok=True)
    k = Ed25519PrivateKey.generate()
    CLE.write_bytes(k.private_bytes(serialization.Encoding.PEM, serialization.PrivateFormat.PKCS8,
                                    serialization.NoEncryption()))
    afficher_publique(k)


def afficher_publique(k):
    pub = k.public_key().public_bytes(serialization.Encoding.Raw, serialization.PublicFormat.Raw)
    print(f"Clé privée : {CLE}")
    print("Clé publique (à mettre dans CLE_PUBLIQUE) :")
    print("[" + ", ".join(f"0x{b:02x}" for b in pub) + "]")


def charger_cle():
    return serialization.load_pem_private_key(CLE.read_bytes(), password=None)


def catalogue_existant():
    local = DIST / "catalogue.json"
    if local.exists():
        return json.loads(local.read_text(encoding="utf-8"))
    try:
        with urllib.request.urlopen(URL_BASE + "catalogue.json", timeout=20) as r:
            return json.loads(r.read().decode("utf-8"))
    except urllib.error.HTTPError as e:
        if e.code == 404:
            return {"format": 1, "extensions": []}
        raise


def paquet(dossier):
    dossier = Path(dossier).resolve()
    manifest = json.loads((dossier / "manifest.json").read_text(encoding="utf-8"))
    id_, version, api = manifest["id"], manifest["version"], manifest["api_version"]
    print(f"Compilation de {id_} {version}…", flush=True)
    subprocess.run(["cargo", "build", "--release"], cwd=dossier, check=True)
    dll = dossier / "target" / "release" / manifest["library"]
    if not dll.exists():
        sys.exit(f"Bibliothèque introuvable : {dll}")

    DIST.mkdir(exist_ok=True)
    nom = f"{id_}-{version}.zip"
    archive = DIST / nom
    with zipfile.ZipFile(archive, "w", zipfile.ZIP_DEFLATED) as z:
        z.write(dossier / "manifest.json", "manifest.json")
        z.write(dll, manifest["library"])
        for f in sorted((dossier / "ui").rglob("*")):
            if f.is_file():
                z.write(f, "ui/" + f.relative_to(dossier / "ui").as_posix())
    octets = archive.read_bytes()
    sha = hashlib.sha256(octets).hexdigest()
    sig = charger_cle().sign(message_signe(id_, version, api, sha))

    cat = catalogue_existant()
    cat["extensions"] = [e for e in cat.get("extensions", []) if e.get("id") != id_]
    cat["extensions"].append({
        "id": id_, "name": manifest["name"], "version": version, "api_version": api,
        "description": manifest.get("description", ""), "size": len(octets), "sha256": sha,
        "signature": base64.b64encode(sig).decode(), "url": URL_BASE + nom,
    })
    cat["extensions"].sort(key=lambda e: e["id"])
    (DIST / "catalogue.json").write_text(json.dumps(cat, ensure_ascii=False, indent=2), encoding="utf-8")
    print(f"{nom} : {len(octets):,} octets, sha256 {sha[:16]}…, signé. Catalogue mis à jour.")


def envoyer():
    """Dépose dist/ sur le VPS. /srv/gws-updates appartient à root : copie dans
    un dossier temporaire, puis `sudo install`. Le catalogue part en dernier,
    une fois les archives en place."""
    archives = sorted(DIST.glob("*.zip"))
    tmp = "/tmp/gws-extensions-envoi"
    subprocess.run(["ssh", HOTE, f"rm -rf {tmp} && mkdir -p {tmp}"], check=True)
    subprocess.run(["scp", "-q", *map(str, archives), str(DIST / "catalogue.json"), f"{HOTE}:{tmp}/"], check=True)
    subprocess.run(["ssh", "-t", HOTE,
                    f"sudo install -d -m 755 {DEST} && "
                    f"sudo install -m 644 {tmp}/*.zip {DEST}/ && "
                    f"sudo install -m 644 {tmp}/catalogue.json {DEST}/ && rm -rf {tmp}"], check=True)
    print(f"{len(archives)} archive(s) et le catalogue déposés dans {DEST}")


if __name__ == "__main__":
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    cmd = sys.argv[1]
    if cmd == "generer-cle":
        generer_cle()
    elif cmd == "cle-publique":
        afficher_publique(charger_cle())
    elif cmd == "paquet" and len(sys.argv) == 3:
        paquet(sys.argv[2])
    elif cmd == "envoyer":
        envoyer()
    else:
        sys.exit(__doc__)
