"""Recensement des stations meteo francaises Infoclimat et test d'acces API Open Data.

Usage :
  python scripts/stations_fr/recenser_stations.py            # lot complet (reprenable)
  python scripts/stations_fr/recenser_stations.py --ids A,B  # test sur quelques IDs
  python scripts/stations_fr/recenser_stations.py --export   # regenere JSON/CSV depuis le checkpoint

Source des IDs : https://www.infoclimat.fr/opendata/stations_xhr.php (JSON utilise par la
carte de la page /opendata/), filtre pays == "FR".
Appels API via le relais SSH de l'app (IP fixe), cle et quota dans %APPDATA%/fr.gastinel.weatherstudio/Clee.API.
Checkpoint : data/stations_fr/checkpoint.jsonl (une ligne par station testee).
La cle API n'est jamais affichee ni ecrite ; token= est masque dans les logs.
"""
import argparse, csv, datetime as dt, json, os, re, sys, time, urllib.error, urllib.parse, urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
OUT = ROOT / "data" / "stations_fr"
CKPT = OUT / "checkpoint.jsonl"
CANDIDATS = OUT / "candidats_fr.json"
APPDIR = Path(os.environ.get("APPDATA", "")) / "fr.gastinel.weatherstudio" / "Clee.API"
QUOTA = APPDIR / "infoclimat_quota.json"
KEYFILE = APPDIR / "API.Infoclimat.env"
# Relais SSH (IP fixe autorisee par Infoclimat), comme src-tauri/src/ssh_relay.rs
# Adresse et compte lus dans l'environnement : rien d'infra en dur dans le depot.
RELAIS_HOTE = os.environ.get("GWS_RELAIS_HOTE", "")
RELAIS_USER = os.environ.get("GWS_RELAIS_USER", "debian")
RELAIS_CLE = Path(os.environ.get("GWS_RELAIS_CLE", Path.home() / ".ssh" / "id_ed25519_relais"))
RELAIS_EMPREINTE = os.environ.get("GWS_RELAIS_EMPREINTE", "")
_ssh = [None]
_secret = [None]
UA = "GastinelWeatherStudio/0.4 (recensement stations)"
PAUSE = 1.1
LISTE_URL = "https://www.infoclimat.fr/opendata/stations_xhr.php"
LICENCES = {0: "Etalab (Licence Ouverte)", 1: "CC BY", 2: "CC BY-NC"}
_last = [0.0]


def masquer(t):
    t = re.sub(r"token=[^&\s'\"]+", "token=***", str(t))
    return t.replace(_secret[0], "***") if _secret[0] else t


def log(msg):
    print(masquer(msg), flush=True)


def relais():
    """Connexion SSH unique, empreinte ED25519 verifiee (toute autre refusee)."""
    if _ssh[0] is None:
        if not RELAIS_HOTE or not RELAIS_EMPREINTE:
            sys.exit("GWS_RELAIS_HOTE et GWS_RELAIS_EMPREINTE doivent etre definis.")
        import base64, hashlib, socket, paramiko
        sock = socket.create_connection((RELAIS_HOTE, 22), timeout=30)
        tr = paramiko.Transport(sock)
        tr.get_security_options().key_types = ["ssh-ed25519"]
        tr.start_client(timeout=30)
        k = tr.get_remote_server_key()
        fp = "SHA256:" + base64.b64encode(hashlib.sha256(k.asbytes()).digest()).decode().rstrip("=")
        if fp != RELAIS_EMPREINTE:
            tr.close()
            sys.exit(f"ARRET : empreinte du relais inattendue {fp}")
        tr.auth_publickey(RELAIS_USER, paramiko.PKey.from_path(str(RELAIS_CLE)))
        tr.set_keepalive(30)
        _ssh[0] = tr
    return _ssh[0]


def relais_get(url):
    """curl execute sur le relais ; renvoie (code HTTP, corps). Espacement >= 1,1 s."""
    import shlex
    for essai in range(5):
        attente = PAUSE - (time.time() - _last[0])
        if attente > 0:
            time.sleep(attente)
        _last[0] = time.time()
        try:
            ch = relais().open_session(timeout=30)
            ch.settimeout(90)
            ch.exec_command("curl -s -m 60 -w '\n%{http_code}' -A " + shlex.quote(UA) + " " + shlex.quote(url))
            out = b""
            while True:
                b = ch.recv(65536)
                if not b:
                    break
                out += b
            err = ch.recv_stderr(4096).decode("utf-8", "replace")
            ch.close()
            txt = out.decode("utf-8", "replace")
            corps, _, code = txt.rpartition(chr(10))
            code = int(code) if code.strip().isdigit() else None
            if code in (429, 503):
                log(f"HTTP {code}, pause 5 min (essai {essai + 1})")
                time.sleep(300)
                continue
            if code is None or code == 0:
                raise RuntimeError("curl sans code HTTP " + masquer(err)[:100])
            return code, corps
        except SystemExit:
            raise
        except Exception as e:
            log(f"Erreur relais {type(e).__name__}: {masquer(e)[:120]}, pause 60 s")
            try:
                _ssh[0] and _ssh[0].close()
            except Exception:
                pass
            _ssh[0] = None
            time.sleep(60)
    return None, "echec apres 5 essais"


def lire_cle():
    v = os.environ.get("INFOCLIMAT_API_KEY")
    if v and v.strip():
        return v.strip()
    for l in KEYFILE.read_text(encoding="utf-8").splitlines():
        l = l.strip()
        if not l or l.startswith("#"):
            continue
        v = l.split("=", 1)[1] if "=" in l else l
        v = v.strip().strip("\"'").strip()
        if v:
            _secret[0] = v
            return v
    sys.exit("Cle Infoclimat introuvable")


def quota_incr():
    """Meme schema que DailyQuotaInfo (src-tauri/src/infoclimat_client.rs)."""
    now = dt.datetime.now().astimezone()
    today = now.strftime("%Y-%m-%d")
    tz = now.strftime("%z")
    tz = tz[:3] + ":" + tz[3:]
    try:
        q = json.loads(QUOTA.read_text(encoding="utf-8"))
    except Exception:
        q = {}
    if q.get("date") != today:
        q = {"date": today, "requests_today": 0, "max_daily_requests": q.get("max_daily_requests", 5000),
             "last_request_utc": None, "timezone_offset": tz}
    if q["requests_today"] >= q.get("max_daily_requests", 5000):
        sys.exit("Quota journalier atteint : arret, relancer demain (reprise automatique).")
    q["requests_today"] += 1
    q["last_request_utc"] = dt.datetime.now(dt.timezone.utc).isoformat()
    QUOTA.write_text(json.dumps(q, indent=2), encoding="utf-8")


def http_get(url):
    """GET poli : >= 1,1 s entre requetes, pause longue sur 429/503."""
    for essai in range(5):
        attente = PAUSE - (time.time() - _last[0])
        if attente > 0:
            time.sleep(attente)
        _last[0] = time.time()
        req = urllib.request.Request(url, headers={"User-Agent": UA})
        try:
            with urllib.request.urlopen(req, timeout=60) as r:
                return r.status, r.read().decode("utf-8", "replace")
        except urllib.error.HTTPError as e:
            if e.code in (429, 503):
                log(f"HTTP {e.code}, pause 5 min (essai {essai + 1})")
                time.sleep(300)
                continue
            return e.code, e.read().decode("utf-8", "replace")
        except Exception as e:
            log(f"Erreur reseau {type(e).__name__}, pause 60 s")
            time.sleep(60)
    return None, "echec apres 5 essais"


def charger_candidats():
    if CANDIDATS.exists():
        return json.loads(CANDIDATS.read_text(encoding="utf-8"))
    code, txt = http_get(LISTE_URL)
    if code != 200:
        sys.exit(f"Liste des stations indisponible (HTTP {code})")
    fr = [s for s in json.loads(txt) if s.get("pays") == "FR"]
    CANDIDATS.write_text(json.dumps(fr, ensure_ascii=False), encoding="utf-8")
    return fr


def reseau(s):
    i, g = s["id"], s.get("genre") or ""
    if g == "synop" or re.fullmatch(r"0[78]\d{3}", i):
        return "OMM/synop"
    if i.startswith("STATIC"):
        return "StatIC"
    if i.startswith("ME"):
        return "StatIC (ME)"
    return "StatIC" if g == "static" else (g or "inconnu")


def tester(sid, cle, meta):
    j = (dt.date.today() - dt.timedelta(days=2)).isoformat()
    url = ("https://www.infoclimat.fr/opendata/?version=2&method=get&format=json&stations[]="
           + urllib.parse.quote(sid) + f"&start={j}&end={j}&token=" + urllib.parse.quote(cle))
    quota_incr()
    code, txt = relais_get(url)
    base = {"id": sid, "nom": meta.get("libelle"), "latitude": meta.get("latitude"),
            "longitude": meta.get("longitude"), "altitude_m": meta.get("altitude"),
            "departement": meta.get("departement"), "reseau": reseau(meta),
            "licence": LICENCES.get(meta.get("licence")), "source": "opendata/stations_xhr.php"}
    if txt.strip().lower().startswith("wrong ip"):
        log("ARRET : 'Wrong ip address' -> la cle n'est pas autorisee pour cette IP.")
        sys.exit(2)
    txt = txt.replace(cle, "***")
    try:
        js = json.loads(txt)
    except Exception:
        return {**base, "api_disponible": False,
                "raison": f"reponse non JSON (HTTP {code}) : {txt.strip()[:120]}"}
    st = [s for s in (js.get("stations") or []) if isinstance(s, dict)] if isinstance(js, dict) else []
    if st:
        s = next((x for x in st if str(x.get("id")) == sid), st[0])
        lic = s.get("license") or s.get("licence")
        if isinstance(lic, dict):
            lic = lic.get("license") or lic.get("name") or json.dumps(lic, ensure_ascii=False)
        return {**base, "nom": s.get("name") or base["nom"],
                "latitude": s.get("latitude", base["latitude"]),
                "longitude": s.get("longitude", base["longitude"]),
                "altitude_m": s.get("elevation", base["altitude_m"]),
                "licence": lic or base["licence"], "api_disponible": True, "raison": None,
                "donnees_recentes": bool((js.get("hourly") or {}).get(sid) or js.get("data"))} if js.get("status", "OK") == "OK" else {
                **base, "api_disponible": False, "raison": f"status {js.get('status')}"}
    err = (js.get("errors") or js.get("error") or js.get("message")) if isinstance(js, dict) else None
    return {**base, "api_disponible": False,
            "raison": f"pas de bloc stations[] (HTTP {code})" + (f" : {str(err)[:150]}" if err else "")}


def exporter():
    res = {}
    if CKPT.exists():
        for l in CKPT.read_text(encoding="utf-8").splitlines():
            if l.strip():
                r = json.loads(l)
                res[r["id"]] = r
    rows = sorted(res.values(), key=lambda r: r["id"])
    (OUT / "stations_fr.json").write_text(json.dumps(rows, ensure_ascii=False, indent=1), encoding="utf-8")
    cols = ["id", "nom", "latitude", "longitude", "altitude_m", "departement", "reseau",
            "api_disponible", "donnees_recentes", "licence", "raison", "source"]
    with open(OUT / "stations_fr.csv", "w", newline="", encoding="utf-8-sig") as f:
        w = csv.DictWriter(f, fieldnames=cols, extrasaction="ignore")
        w.writeheader()
        w.writerows(rows)
    ok = sum(1 for r in rows if r["api_disponible"])
    log(f"Export : {len(rows)} stations, {ok} disponibles, {len(rows) - ok} indisponibles")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--ids")
    ap.add_argument("--export", action="store_true")
    a = ap.parse_args()
    OUT.mkdir(parents=True, exist_ok=True)
    if a.export:
        return exporter()
    cle = lire_cle()
    cands = {s["id"]: s for s in charger_candidats()}
    faits = set()
    if CKPT.exists():
        faits = {json.loads(l)["id"] for l in CKPT.read_text(encoding="utf-8").splitlines() if l.strip()}
    ids = a.ids.split(",") if a.ids else list(cands)
    todo = [i for i in ids if i not in faits]
    log(f"{len(cands)} candidats FR, {len(faits)} deja traites, {len(todo)} a tester")
    with open(CKPT, "a", encoding="utf-8") as f:
        for n, sid in enumerate(todo, 1):
            r = tester(sid, cle, cands.get(sid, {"id": sid}))
            f.write(json.dumps(r, ensure_ascii=False) + "\n")
            f.flush()
            log(f"[{n}/{len(todo)}] {sid} -> {'OK' if r['api_disponible'] else 'KO ' + str(r['raison'])[:100]}")
    exporter()


if __name__ == "__main__":
    main()
