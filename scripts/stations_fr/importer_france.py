"""Import par lots de l'historique Infoclimat collecté dans le projet « France ».

Lit les fichiers `<station>/<debut>_<fin>.json.gz` rapatriés sur CornafServer
(copie locale : --source) et écrit directement dans la base SQLite de l'app.

Niveau de détail « mixte » :
  * départements listés dans --detail (défaut 38,26 : Vercors) → toutes les
    mesures, pas de 10 min ;
  * autres départements → résumé horaire. La température stockée est le
    **minimum** de l'heure (l'app sert à chercher le froid : les records et les
    minima restent exacts), humidité/pression/vent moyen en moyenne, rafale en
    maximum.

Sûreté :
  * refuse de tourner si l'app est ouverte ;
  * sauvegarde la base avant la première écriture ;
  * une transaction par station ; une station déjà importée est sautée, donc
    le script peut être relancé après une interruption ;
  * INSERT OR IGNORE sur l'index unique (series_id, timestamp_utc) : les
    fenêtres qui se chevauchent ne créent pas de doublons.

Import complet (prompt 37) : `--detail all` garde toutes les stations au pas de
10 min. Une série existante de la station dont le pas diffère (ex. résumé horaire)
est supprimée dans la même transaction que l'insertion de la série 10 min
(remplacement sans doublon). Seules les stations dont la collecte est terminée
(un fichier se terminant à --fin-collecte, détectée automatiquement = date de fin
la plus récente) sont importées. `--index-apres` supprime l'index unique
(series_id, timestamp_utc) pendant l'insertion (dédoublonnage fait en Python) et
le recrée à la fin ; une relance après interruption le recrée aussi.

Usage :
  python importer_france.py --source D:/infoclimat [--detail 38,26|all] [--limite N] [--db chemin]
  python importer_france.py --source D:/infoclimat --detail all --index-apres
"""

import argparse
import gzip
import json
import os
import shutil
import sqlite3
import subprocess
import sys
import time
import uuid
from datetime import datetime, timezone
from pathlib import Path

ICI = Path(__file__).resolve().parent
DONNEES = ICI.parent.parent / "data" / "stations_fr"
DB_DEFAUT = Path(os.environ["APPDATA"]) / "fr.gastinel.weatherstudio" / "gastinel-weather-studio.sqlite3"
PROJET = "France"


def f(v):
    if v is None or v == "":
        return None
    try:
        return float(v)
    except (TypeError, ValueError):
        return None


def ts(dh):
    return int(datetime.strptime(dh, "%Y-%m-%d %H:%M:%S").replace(tzinfo=timezone.utc).timestamp())


def app_ouverte():
    try:
        sortie = subprocess.run(["tasklist"], capture_output=True, text=True, errors="ignore").stdout
    except OSError:
        return False
    return "gastinel-weather-studio" in sortie.lower()


INDEX_UNIQUE = "idx_measurements_series_ts_unique"
SQL_INDEX = f"CREATE UNIQUE INDEX IF NOT EXISTS {INDEX_UNIQUE} ON measurements(series_id, timestamp_utc)"


# État mensuel des séries (schéma v10 de l'app). Copie EXACTE de `TRIGGERS_V10`
# (trigger d'ajout) et de `REPARER_SERIES_MONTHS` dans src-tauri/src/db.rs.
TRIGGER_AJOUT = "trg_mesures_ajout"
SQL_TRIGGER_AJOUT = """
CREATE TRIGGER IF NOT EXISTS trg_mesures_ajout AFTER INSERT ON measurements BEGIN
    INSERT INTO series_months (series_id, month, n, version)
    VALUES (NEW.series_id, CAST(strftime('%Y%m', NEW.timestamp_utc, 'unixepoch') AS INTEGER), 1, 1)
    ON CONFLICT (series_id, month) DO UPDATE SET n = n + 1, version = version + 1;
END;
"""
SQL_REPARER_MOIS = """
INSERT OR REPLACE INTO series_months (series_id, month, n, version)
    SELECT m.series_id, CAST(strftime('%Y%m', m.timestamp_utc, 'unixepoch') AS INTEGER), count(*),
           CAST(strftime('%s', 'now') AS INTEGER)
    FROM measurements m
    WHERE m.series_id IN (
        SELECT s.id FROM logger_series s
        WHERE NOT EXISTS (SELECT 1 FROM series_months x WHERE x.series_id = s.id)
    )
    GROUP BY 1, 2;
"""


def schema_v10(c):
    """La base a-t-elle la table series_months (app 0.4.2 et plus) ?"""
    return c.execute("SELECT 1 FROM sqlite_master WHERE type='table' AND name='series_months'").fetchone() is not None


def fin_fichier(fichier):
    return fichier.name.split("_")[-1].replace(".json.gz", "")


def detecter_fin(source):
    fins = [fin_fichier(f) for f in source.glob("*/*.json.gz")]
    return max(fins) if fins else ""


def station_complete(dossier, fin):
    return any(fin_fichier(f) == fin for f in dossier.glob("*.json.gz"))


def lire_station(dossier, sid):
    """Enregistrements (ts, temp, hum, pres, vent, rafale, dir) + licence, fichier par fichier."""
    licence = None
    for fichier in sorted(dossier.glob("*.json.gz")):
        with gzip.open(fichier, "rt", encoding="utf-8") as g:
            d = json.load(g)
        if licence is None:
            for st in d.get("stations") or []:
                licence = (st.get("license") or {}).get("license")
        for r in (d.get("hourly") or {}).get(sid) or []:
            t = f(r.get("temperature"))
            if t is None or not r.get("dh_utc"):
                continue
            try:
                instant = ts(r["dh_utc"])
            except ValueError:
                continue
            rafale = f(r.get("vent_rafales")) or f(r.get("vent_rafales_10min"))
            yield licence, (instant, t, f(r.get("humidite")), f(r.get("pression")),
                            f(r.get("vent_moyen")), rafale, f(r.get("vent_direction")))


def moyenne(vals):
    vals = [v for v in vals if v is not None]
    return sum(vals) / len(vals) if vals else None


def resumer_horaire(enregs):
    heures = {}
    for e in enregs:
        heures.setdefault(e[0] // 3600 * 3600, []).append(e)
    for h in sorted(heures):
        g = heures[h]
        rafales = [e[5] for e in g if e[5] is not None]
        yield (h, min(e[1] for e in g), moyenne(e[2] for e in g), moyenne(e[3] for e in g),
               moyenne(e[4] for e in g), max(rafales) if rafales else None, g[-1][6])


def creer_point(c, projet, st, d, maintenant):
    """Point du projet pour une station, avec sa description (agent 2)."""
    point_id = str(uuid.uuid4())
    categorie = d.get("category") or "Cat2"
    taille = d.get("size")
    # Mêmes règles que la contrainte CHECK de la table `points`.
    if categorie in ("Cat1", "Col", "Versants"):
        taille = None
    elif categorie == "Cat2" and taille not in (None, "Gr"):
        taille = None
    elif categorie in ("Cat3", "Cat4") and taille not in ("Gr", "Pt"):
        taille = "Pt"
    c.execute(
        """INSERT INTO points (id, project_id, name, latitude, longitude, altitude_m, category, size,
             factor_sky, factor_wind, factor_slope, factor_snow_cover, factor_forest_cover,
             is_open_terrain, notes, created_at, infoclimat_station_id)
           VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)""",
        (point_id, projet, st["nom"], st["latitude"], st["longitude"], st.get("altitude_m"),
         categorie, taille, d.get("factor_sky"), d.get("factor_wind"),
         d.get("factor_slope"), d.get("factor_snow_cover"), d.get("factor_forest_cover"),
         None if d.get("is_open_terrain") is None else int(d["is_open_terrain"]),
         d.get("notes"), maintenant, st["id"]))
    return point_id


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--source", default="", help="dossier contenant un sous-dossier par station")
    ap.add_argument("--db", default=str(DB_DEFAUT))
    ap.add_argument("--detail", default="38,26", help="départements gardés au pas de 10 min")
    ap.add_argument("--limite", type=int, default=0, help="nombre max de stations (test)")
    ap.add_argument("--sans-mesures", action="store_true",
                    help="crée seulement les points (toutes les stations), sans aucune série")
    ap.add_argument("--fin-collecte", default="",
                    help="date AAAA-MM-JJ de fin de collecte (défaut : la plus récente des fichiers)")
    ap.add_argument("--index-apres", action="store_true",
                    help="supprime l'index unique pendant l'import et le recrée à la fin")
    ap.add_argument("--sans-sauvegarde", action="store_true", help="tests sur copie uniquement")
    ap.add_argument("--stations", default="", help="n'importer les mesures que de ces identifiants (séparés par des virgules)")
    args = ap.parse_args()

    # Seule la vraie base est partagée avec l'app ; une copie de test ne l'est pas.
    if Path(args.db).resolve() == DB_DEFAUT.resolve() and app_ouverte():
        sys.exit("L'app GastinelWeatherStudio est ouverte : ferme-la avant l'import.")

    source = Path(args.source)
    detail = {d.strip() for d in args.detail.split(",") if d.strip()}
    tout_detail = "all" in detail
    stations = json.load(open(DONNEES / "stations_fr.json", encoding="utf-8"))
    descr = {d["id"]: d for d in json.load(open(DONNEES / "descriptions_fr.json", encoding="utf-8"))}

    sauvegarde = Path(args.db).with_name(
        Path(args.db).stem + f".avant-import-france-{time.strftime('%Y%m%d-%H%M%S')}.sqlite3")
    c = sqlite3.connect(args.db)
    c.execute("PRAGMA foreign_keys=ON")
    c.execute("PRAGMA journal_mode=WAL")
    c.execute("PRAGMA synchronous=NORMAL")
    c.execute("PRAGMA cache_size=-262144")
    c.execute("PRAGMA wal_checkpoint(TRUNCATE)")
    if args.sans_sauvegarde and Path(args.db).resolve() == DB_DEFAUT.resolve():
        sys.exit("--sans-sauvegarde est réservé aux copies de test.")
    if not args.sans_sauvegarde:
        c.execute(f"VACUUM INTO '{sauvegarde.as_posix()}'")
        print(f"Sauvegarde : {sauvegarde}")

    row = c.execute("SELECT id FROM projects WHERE name=?", (PROJET,)).fetchone()
    if not row:
        sys.exit(f"Projet « {PROJET} » introuvable dans la base.")
    projet = row[0]
    # station -> [(series_id, pas)] des séries Infoclimat déjà présentes dans le projet
    existantes = {}
    for sid_, serie_, pas_ in c.execute(
            """SELECT p.infoclimat_station_id, s.id, s.sample_interval_seconds FROM points p
               JOIN logger_series s ON s.point_id=p.id
               WHERE p.project_id=? AND s.source_type='InfoclimatApi'""", (projet,)):
        existantes.setdefault(sid_, []).append((serie_, pas_))

    faites = total = 0
    debut = time.time()
    if args.sans_mesures:
        crees = 0
        with c:
            for st in stations:
                if c.execute("SELECT 1 FROM points WHERE project_id=? AND infoclimat_station_id=?",
                             (projet, st["id"])).fetchone():
                    continue
                creer_point(c, projet, st, descr.get(st["id"], {}), int(time.time()))
                crees += 1
        c.execute("PRAGMA wal_checkpoint(TRUNCATE)")
        c.close()
        print(f"Points créés : {crees} (déjà présents : {len(stations) - crees})")
        return
    if not args.source:
        sys.exit("--source est obligatoire pour importer des mesures.")
    choix = {x.strip() for x in args.stations.split(",") if x.strip()}
    fin = args.fin_collecte or detecter_fin(source)
    print(f"Fin de collecte retenue : {fin}")

    a_faire, incompletes = [], 0
    for st in stations:
        sid = st["id"]
        dossier = source / sid
        if not dossier.is_dir() or (choix and sid not in choix):
            continue
        if not station_complete(dossier, fin):
            incompletes += 1
            continue
        pas = 600 if (tout_detail or st.get("departement") in detail) else 3600
        if any(p == pas for _, p in existantes.get(sid, [])):
            continue  # déjà importée à ce pas : reprise / idempotence
        a_faire.append((st, pas))
    if args.limite:
        a_faire = a_faire[:args.limite]
    print(f"Stations à importer : {len(a_faire)} (collecte incomplète ignorée : {incompletes})")

    # Remplacement : les séries de ces stations à un autre pas (ex. résumé horaire)
    # sont supprimées AVANT de retirer l'index (sinon DELETE = balayage complet).
    remplacees = 0
    with c:
        for st, pas in a_faire:
            for ancienne, p in existantes.get(st["id"], []):
                if p != pas:
                    c.execute("DELETE FROM measurements WHERE series_id=?", (ancienne,))
                    c.execute("DELETE FROM logger_series WHERE id=?", (ancienne,))
                    remplacees += 1
    if remplacees:
        print(f"Séries remplacées (autre pas supprimé) : {remplacees}")

    if args.index_apres and a_faire:
        c.execute(f"DROP INDEX IF EXISTS {INDEX_UNIQUE}")
        print("Index unique supprimé pendant l'import (recréé à la fin).")
    if a_faire and schema_v10(c):
        # Le trigger d'ajout double le temps d'insertion (mesuré : x2,0 sur
        # 1 M lignes). Il est remis à la fin, et l'état des séries importées
        # reconstruit ; si le script est interrompu, l'app le fait elle-même à
        # sa prochaine ouverture (voir `TRIGGERS_V10` dans db.rs).
        c.execute(f"DROP TRIGGER IF EXISTS {TRIGGER_AJOUT}")
        print("Trigger series_months retiré pendant l'import (remis à la fin).")

    for st, pas in a_faire:
        sid = st["id"]
        dossier = source / sid
        t0 = time.time()
        complet = pas == 600
        enregs, licence = [], None
        for lic, e in lire_station(dossier, sid):
            licence = licence or lic
            enregs.append(e)
        if not enregs:
            continue
        if complet:
            # dédoublonnage (fenêtres qui se chevauchent) : un seul relevé par instant
            lignes = sorted({e[0]: e for e in enregs}.values())
        else:
            lignes = list(resumer_horaire(enregs))
        del enregs

        d = descr.get(sid, {})
        maintenant = int(time.time())
        with c:
            pt = c.execute("SELECT id FROM points WHERE project_id=? AND infoclimat_station_id=?",
                           (projet, sid)).fetchone()
            point_id = pt[0] if pt else creer_point(c, projet, st, d, maintenant)

            serie = str(uuid.uuid4())
            c.execute(
                """INSERT INTO logger_series (id, point_id, source_type, role, imported_at, raw_timestamp_was_utc,
                     label, sample_interval_seconds, data_license, station_id)
                   VALUES (?,?,'InfoclimatApi','Primary',?,1,?,?,?,?)""",
                (serie, point_id, maintenant,
                 "Infoclimat — historique complet" if complet else "Infoclimat — résumé horaire (minimum)",
                 600 if complet else 3600, licence, sid))
            c.executemany(
                """INSERT OR IGNORE INTO measurements (series_id, timestamp_utc, raw_timestamp, temperature_celsius,
                     humidity_percent, pressure_hpa, wind_avg_kmh, wind_gust_kmh, wind_direction_degrees)
                   VALUES (?,?,?,?,?,?,?,?,?)""",
                ((serie, l[0], l[0], l[1], l[2], l[3], l[4], l[5], l[6]) for l in lignes))
        faites += 1
        total += len(lignes)
        ecoule = time.time() - debut
        reste = ecoule / faites * (len(a_faire) - faites)
        print(f"[{faites}/{len(a_faire)}] {sid:8s} {'10 min' if complet else 'horaire':7s} "
              f"{len(lignes):>9,} lignes {time.time()-t0:5.1f}s  total {total:,} "
              f"({total/max(ecoule,1e-9):,.0f} l/s, reste ~{reste/60:.0f} min)", flush=True)
        if faites % 25 == 0:
            c.execute("PRAGMA wal_checkpoint(TRUNCATE)")

    if not c.execute("SELECT 1 FROM sqlite_master WHERE name=?", (INDEX_UNIQUE,)).fetchone():
        t0 = time.time()
        print("Création de l'index unique…", flush=True)
        c.execute(SQL_INDEX)
        print(f"Index créé en {time.time()-t0:.0f}s")
    if schema_v10(c):
        t0 = time.time()
        print("Reconstruction de l'état mensuel (series_months)…", flush=True)
        with c:
            c.execute(SQL_REPARER_MOIS)
            c.execute(SQL_TRIGGER_AJOUT)
        print(f"État reconstruit en {time.time()-t0:.0f}s")
    c.execute("PRAGMA wal_checkpoint(TRUNCATE)")
    c.execute("PRAGMA optimize")
    c.close()
    print(f"Terminé : {faites} stations, {total:,} lignes en {time.time()-debut:.0f}s")


if __name__ == "__main__":
    main()
