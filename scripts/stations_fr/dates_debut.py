"""Date de premiere mesure de chaque station de data/stations_fr/stations_fr.json.

Usage :
  python scripts/stations_fr/dates_debut.py            # traite (reprenable, s'arrete a 4500 req/jour)
  python scripts/stations_fr/dates_debut.py --export   # regenere dates_debut.json / stations_fr_dates.csv

Methode : la limite serveur porte sur le NOMBRE DE LIGNES ("Too many lines of data requested"),
pas sur la duree : ~1 an passe pour une station a 10 min, 18 mois non (seuil ~53-79 k lignes).
Dichotomie sur les annees 2000 -> annee courante (fenetre 1 an ; l'erreur "Too many lines"
vaut "a des donnees") puis le premier enregistrement de la premiere annee non vide donne le mois.
Pour limiter le transfert, la reponse est tronquee sur le relais (head -c) : seuls les
premiers enregistrements (dh_utc) sont rapatries, ce qui suffit pour la date et le pas.
"""
import argparse, csv, datetime as dt, json, re, shlex, statistics, sys, time, urllib.parse
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import recenser_stations as r  # noqa: E402

OUT = r.OUT
CKPT = OUT / "dates_debut_checkpoint.jsonl"
PLAFOND = 4500
ANNEE0 = 2000
OCTETS = 300_000


def quota_ok():
    try:
        q = json.loads(r.QUOTA.read_text(encoding="utf-8"))
    except Exception:
        return True
    return q.get("date") != dt.date.today().isoformat() or q.get("requests_today", 0) < PLAFOND


def sonder(sid, cle, a, b):
    """Renvoie (liste dh_utc, nom API, erreur)."""
    if not quota_ok():
        r.log(f"Plafond {PLAFOND} requetes/jour atteint : arret propre (relancer demain).")
        sys.exit(0)
    b = min(b, dt.date.today())
    url = ("https://www.infoclimat.fr/opendata/?version=2&method=get&format=json&stations[]="
           + urllib.parse.quote(sid) + f"&start={a}&end={b}&token=" + urllib.parse.quote(cle))
    r.quota_incr()
    for essai in range(5):
        attente = r.PAUSE - (time.time() - r._last[0])
        if attente > 0:
            time.sleep(attente)
        r._last[0] = time.time()
        try:
            ch = r.relais().open_session(timeout=30)
            ch.settimeout(200)
            ch.exec_command("curl -s -m 180 -A " + shlex.quote(r.UA) + " " + shlex.quote(url)
                            + f" | head -c {OCTETS}")
            out = b""
            while True:
                x = ch.recv(65536)
                if not x:
                    break
                out += x
            ch.close()
            txt = out.decode("utf-8", "replace").replace(cle, "***")
            break
        except Exception as e:
            r.log(f"Erreur relais {type(e).__name__}: {r.masquer(e)[:120]}, pause 60 s")
            try:
                r._ssh[0] and r._ssh[0].close()
            except Exception:
                pass
            r._ssh[0] = None
            time.sleep(60)
    else:
        return [], None, "echec relais"
    if txt.strip().lower().startswith("wrong ip"):
        sys.exit("ARRET : Wrong ip address")
    if "Too many requests" in txt[:500] or not txt.strip():
        r.log("Reponse vide / limitation : pause 5 min")
        time.sleep(300)
        return [], None, "vide"
    m = re.search(r'"errors":\s*\[\s*"([^"]+)"', txt)
    if m:
        return [], None, m.group(1)
    nom = re.search(r'"name":\s*"((?:[^"\\]|\\.)*)"', txt)
    nom = json.loads('"' + nom.group(1) + '"') if nom else None
    return re.findall(r'"dh_utc":\s*"([^"]+)"', txt), nom, None


def pas_minutes(dhs):
    t = [dt.datetime.fromisoformat(x) for x in dhs]
    d = [(b - a).total_seconds() / 60 for a, b in zip(t, t[1:]) if b > a]
    return round(statistics.median(d)) if d else None


def dater(sid, cle, actif=True):
    res = {"id": sid, "date_debut": None, "derniere_mesure": None, "pas_minutes": None, "appels_utilises": 0}
    cache = {}

    def sonde(a, b):
        if (a, b) not in cache:
            res["appels_utilises"] += 1
            dhs, nom, err = sonder(sid, cle, a, b)
            if nom:
                res["nom_api"] = nom
            if err and "Too many lines" not in err:
                res["erreur"] = err
            cache[(a, b)] = (dhs, bool(err and "Too many lines" in err))
        return cache[(a, b)]

    def an(y):
        return sonde(dt.date(y, 1, 1), dt.date(y, 12, 31))

    def a_donnees(y):
        dhs, trop = an(y)
        return bool(dhs) or trop

    # premiere annee avec donnees (dichotomie, donnees supposees continues apres le debut)
    lo, hi = ANNEE0, dt.date.today().year
    if not actif:  # station arretee : balayage grossier (pas de 3 ans) pour trouver une annee non vide
        for y in range(ANNEE0 + 1, hi + 1, 3):
            if a_donnees(y):
                lo, hi = max(ANNEE0, y - 2), y
                break
        else:
            res["remarque"] = "aucune donnee trouvee (sondes tous les 3 ans)"
            return res
    while lo < hi:
        m = (lo + hi) // 2
        if a_donnees(m):
            hi = m
        else:
            lo = m + 1
    dhs, trop = an(lo)
    for jours in (120, 30):  # annee trop dense : fenetre plus courte en debut d'annee
        if dhs or not trop:
            break
        dhs, trop = sonde(dt.date(lo, 1, 1), dt.date(lo, 1, 1) + dt.timedelta(days=jours))
    if not dhs and trop:
        res["date_debut"] = f"{lo}-01"
        res["remarque"] = "date au plus tard : annee trop dense pour etre lue"
    if dhs:
        res["date_debut"] = dhs[0][:7]
        res["premier_enregistrement"] = dhs[0]
        res["pas_minutes"] = pas_minutes(dhs)
    return res


def charger_ckpt():
    d = {}
    if CKPT.exists():
        for l in CKPT.read_text(encoding="utf-8").splitlines():
            if l.strip():
                x = json.loads(l)
                d[x["id"]] = x
    return d


def exporter():
    st = json.loads((OUT / "stations_fr.json").read_text(encoding="utf-8"))
    ck = charger_ckpt()
    rows = [ck[s["id"]] for s in st if s["id"] in ck]
    (OUT / "dates_debut.json").write_text(json.dumps(rows, ensure_ascii=False, indent=1), encoding="utf-8")
    cols = ["date_debut", "derniere_mesure", "pas_minutes", "appels_utilises"]
    with open(OUT / "stations_fr.csv", encoding="utf-8-sig", newline="") as f:
        src = list(csv.DictReader(f))
    with open(OUT / "stations_fr_dates.csv", "w", encoding="utf-8-sig", newline="") as f:
        w = csv.DictWriter(f, fieldnames=list(src[0].keys()) + cols)
        w.writeheader()
        for s in src:
            x = ck.get(s.get("id"), {})
            if x.get("nom_api") and "�" in (s.get("nom") or ""):
                s["nom"] = x["nom_api"]
            w.writerow({**s, **{c: x.get(c) for c in cols}})
    print(f"export : {len(rows)} stations datees / {len(st)}")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--export", action="store_true")
    a = ap.parse_args()
    if a.export:
        return exporter()
    cle = r.lire_cle()
    st = json.loads((OUT / "stations_fr.json").read_text(encoding="utf-8"))
    fait = {k: v for k, v in charger_ckpt().items() if v.get("date_debut") or v.get("remarque")}
    st_ordre = sorted(st, key=lambda s: not s.get("donnees_recentes"))  # actives d'abord
    try:
        for n, s in enumerate(st_ordre):
            if s["id"] in fait:
                continue
            x = dater(s["id"], cle, bool(s.get("donnees_recentes")))
            x["active"] = bool(s.get("donnees_recentes"))
            with open(CKPT, "a", encoding="utf-8") as f:
                f.write(json.dumps(x, ensure_ascii=False) + "\n")
            r.log(f"[{n + 1}/{len(st)}] {s['id']} debut={x['date_debut']} pas={x['pas_minutes']} appels={x['appels_utilises']}")
    finally:
        exporter()


if __name__ == "__main__":
    main()
