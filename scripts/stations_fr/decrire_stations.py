"""Génère data/stations_fr/descriptions_fr.json à partir de candidats_fr.json.

Étape 1 (enrichir) : commune via geo.api.gouv.fr (contenance), altitudes IGN
(RGE ALTI, data.geopf.fr) au centre et sur deux couronnes (1 km et 3 km, 8 dir.).
Cache reprenable : data/stations_fr/enrichissement_cache.json.
Étape 2 (decrire) : classification topographique par règles + rédaction des textes.

Usage : python decrire_stations.py [enrichir|decrire|tout]
N'appelle jamais infoclimat.fr.
"""
import json, math, sys, time, urllib.request, urllib.parse
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2] / "data" / "stations_fr"
SRC = ROOT / "candidats_fr.json"
CACHE = ROOT / "enrichissement_cache.json"
OUT = ROOT / "descriptions_fr.json"
DIRS = ["N", "NE", "E", "SE", "S", "SO", "O", "NO"]
RADII = (1000, 3000)
GEOPF = "https://data.geopf.fr/altimetrie/1.0/calcul/alti/rest/elevation.json"


def http_json(url, data=None, tries=4):
    for i in range(tries):
        try:
            req = urllib.request.Request(url, data=data, headers={"Content-Type": "application/json", "User-Agent": "GastinelWeatherStudio/stations"})
            with urllib.request.urlopen(req, timeout=60) as r:
                return json.loads(r.read().decode("utf-8"))
        except Exception as e:  # noqa
            time.sleep(1.5 * (i + 1))
            err = e
    raise err


def ring_points(lat, lon):
    pts = [(lon, lat)]
    for r in RADII:
        for k in range(8):
            a = math.radians(k * 45)
            dlat = r * math.cos(a) / 111320
            dlon = r * math.sin(a) / (111320 * math.cos(math.radians(lat)))
            pts.append((lon + dlon, lat + dlat))
    return pts


def enrichir():
    st = json.loads(SRC.read_text(encoding="utf-8"))
    cache = json.loads(CACHE.read_text(encoding="utf-8")) if CACHE.exists() else {}
    todo = [s for s in st if s["id"] not in cache or "alt" not in cache[s["id"]]]
    print(f"altitudes à calculer : {len(todo)}")
    for i in range(0, len(todo), 20):  # 20 stations x 17 pts par requête
        lot = todo[i:i + 20]
        pts = [p for s in lot for p in ring_points(s["latitude"], s["longitude"])]
        body = json.dumps({"lon": "|".join(f"{p[0]:.6f}" for p in pts), "lat": "|".join(f"{p[1]:.6f}" for p in pts),
                           "resource": "ign_rge_alti_wld", "zonly": "true"}).encode()
        el = http_json(GEOPF, body)["elevations"]
        for j, s in enumerate(lot):
            cache.setdefault(s["id"], {})["alt"] = el[j * 17:(j + 1) * 17]
        CACHE.write_text(json.dumps(cache), encoding="utf-8")
        time.sleep(0.3)
    todo = [s for s in st if "commune" not in cache.get(s["id"], {})]
    print(f"communes à chercher : {len(todo)}")
    for n, s in enumerate(todo):
        q = urllib.parse.urlencode({"lat": s["latitude"], "lon": s["longitude"], "fields": "nom,codeDepartement,departement,population", "format": "json"})
        try:
            r = http_json("https://geo.api.gouv.fr/communes?" + q)
        except Exception:
            r = []
        cache[s["id"]]["commune"] = r[0] if r else None
        if n % 50 == 0:
            CACHE.write_text(json.dumps(cache), encoding="utf-8")
        time.sleep(0.22)
    CACHE.write_text(json.dumps(cache), encoding="utf-8")


# ---------------------------------------------------------------- description
MASSIFS = {  # départements -> contexte régional sommaire
    "Alpes": {"04", "05", "06", "38", "73", "74"}, "Pyrénées": {"09", "31", "64", "65", "66"},
    "Massif central": {"03", "12", "15", "19", "23", "43", "48", "63", "87", "42"},
    "Jura": {"01", "25", "39"}, "Vosges": {"88", "68", "67", "70", "90"}, "Corse": {"2A", "2B", "20"},
}


def region(dep):
    for k, v in MASSIFS.items():
        if dep in v:
            return k
    return None


def valid(v):
    return v is not None and v > -1000


def classer(s, alt):
    h0 = alt[0] if valid(alt[0]) else s["altitude"]
    r1 = [a for a in alt[1:9]]
    r3 = [a for a in alt[9:17]]
    sea = sum(1 for a in r1 + r3 if a is None or a <= 0.5)
    r1v = [a if valid(a) else 0 for a in r1]
    r3v = [a if valid(a) else 0 for a in r3]
    d1 = [a - h0 for a in r1v]
    d3 = [a - h0 for a in r3v]
    tpi1, tpi3 = h0 - sum(r1v) / 8, h0 - sum(r3v) / 8
    amp1, amp3 = max(r1v) - min(r1v), max(r3v + [h0]) - min(r3v + [h0])
    up1 = sum(1 for d in d1 if d > 10)
    up3 = sum(1 for d in d3 if d > 25)
    # col : deux directions opposées plus hautes, perpendiculaires plus basses
    col = False
    for k in range(4):
        a, b, c, d = d1[k], d1[k + 4], d1[(k + 2) % 8], d1[(k + 6) % 8]
        if min(a, b) > 40 and max(c, d) < -30:
            col = True
    info = dict(h0=round(h0), tpi1=round(tpi1), tpi3=round(tpi3), amp1=round(amp1), amp3=round(amp3), up1=up1, up3=up3, sea=sea)
    haut = h0 >= 1400
    if col:
        return "Col", None, "moyenne", "col", info
    if up1 == 8 and up3 >= 7 and tpi3 < -40:
        cat = "Cat4" if haut else "Cat3"
        return cat, "Gr" if amp3 > 150 else "Pt", "moyenne", "cuvette_fermee", info
    if up1 == 8 and tpi1 < -8:
        cat = "Cat4" if haut else "Cat3"
        return cat, "Pt", "basse", "cuvette_fermee", info
    if tpi1 > 40 and up1 <= 1:
        return "Versants", None, "basse", "sommet", info
    if amp1 > 120 and tpi1 > -25:
        return "Versants", None, "haute" if amp1 > 250 else "moyenne", "versant", info
    if up1 >= 6 and tpi3 < -30:
        return "Cat2", ("Gr" if amp3 > 250 else None), "moyenne", "cuvette_ouverte", info
    if amp3 > 150 and tpi3 < -20:
        return "Cat1", None, "moyenne", "fond_vallee", info
    if amp3 < 60:
        return "Cat1", None, "haute", "plaine", info
    return "Cat1", None, "basse", "plateau", info


def expo(alt):
    """Direction vers laquelle le terrain descend (exposition du versant)."""
    r1 = [a if valid(a) else 0 for a in alt[1:9]]
    gx = sum(r1[k] * math.sin(math.radians(45 * k)) for k in range(8))
    gy = sum(r1[k] * math.cos(math.radians(45 * k)) for k in range(8))
    if abs(gx) + abs(gy) < 1:
        return None
    ang = (math.degrees(math.atan2(-gx, -gy)) + 360) % 360
    return DIRS[int((ang + 22.5) // 45) % 8]


TOPO_TXT = {
    "col": "au niveau d'un col ou d'un ensellement, entre deux reliefs",
    "cuvette_fermee": "au fond d'une cuvette fermée, entourée de terrains plus élevés dans toutes les directions",
    "cuvette_ouverte": "dans une dépression largement entourée de reliefs mais ouverte vers l'aval",
    "sommet": "sur un point haut (sommet, crête ou butte) dominant les environs",
    "versant": "sur un versant",
    "fond_vallee": "en fond de vallée ou en bas de pente, dominé par les reliefs voisins",
    "plaine": "en terrain plat et ouvert (plaine ou plateau sans relief marqué)",
    "plateau": "sur un plateau ou une campagne vallonnée, relief modéré",
}
IMPL_TXT = {
    "col": "Site exposé au vent canalisé ; brassage fréquent, inversions nocturnes peu marquées.",
    "cuvette_fermee": "L'air froid s'y accumule par nuit claire et calme : site propice aux inversions et aux minimales très basses.",
    "cuvette_ouverte": "Accumulation d'air froid possible par nuit calme, mais drainage partiel vers l'aval : inversions modérées.",
    "sommet": "Site ventilé, généralement au-dessus des inversions nocturnes ; minimales relativement douces pour l'altitude.",
    "versant": "Les brises de pente drainent l'air froid vers le bas : la station se situe souvent dans la ceinture thermique, avec des minimales modérées.",
    "fond_vallee": "Réceptacle de l'air froid descendu des versants : inversions nocturnes fréquentes par temps calme, drainage selon l'axe de la vallée.",
    "plaine": "Refroidissement radiatif de plaine ouverte, sans piégeage topographique notable ; vent peu freiné par le relief.",
    "plateau": "Inversions faibles à modérées selon les creux locaux ; situation représentative des conditions régionales.",
}


def decrire():
    st = json.loads(SRC.read_text(encoding="utf-8"))
    cache = json.loads(CACHE.read_text(encoding="utf-8"))
    out = []
    for s in st:
        c = cache.get(s["id"], {})
        alt = c.get("alt") or [s["altitude"]] * 17
        cat, size, conf, topo, info = classer(s, alt)
        com = c.get("commune") or {}
        commune = com.get("nom")
        dep_nom = (com.get("departement") or {}).get("nom") if isinstance(com.get("departement"), dict) else None
        dep = com.get("codeDepartement") or s.get("departement")
        pop = com.get("population") or 0
        nom = s["libelle"]
        low = nom.lower()
        littoral = info["sea"] >= 2
        h = info["h0"]
        reg = region(dep) if (h >= 400 or dep in MASSIFS["Corse"]) else None
        ex = expo(alt)
        urbain = pop >= 50000 or any(k in low for k in ("ville", "centre", "urbain"))
        aero = any(k in low for k in ("aéroport", "aeroport", "aérodrome", "aerodrome", "base aérienne"))
        # phrase 1 : situation
        lieu = f"Station « {nom} »" + (f", commune de {commune}" if commune else "") + (f" ({dep_nom or dep})" if dep else "")
        ctx = []
        if reg:
            ctx.append(f"massif/région : {reg}")
        if littoral:
            ctx.append("proximité immédiate de la mer")
        p1 = f"{lieu}, à {h} m d'altitude" + (f" ({', '.join(ctx)})" if ctx else "") + "."
        t = TOPO_TXT[topo]
        if topo == "versant" and ex:
            t += f" exposé {ex}"
        p2 = f"Située {t} (écart au relief moyen : {info['tpi1']:+d} m à 1 km, {info['tpi3']:+d} m à 3 km ; dénivelé de {info['amp3']} m dans un rayon de 3 km)."
        env = []
        if aero:
            env.append("Environnement d'aérodrome, terrain dégagé et engazonné.")
        elif urbain:
            env.append(f"Environnement urbain ({pop:,} hab.".replace(",", " ") + ") : effet d'îlot de chaleur probable sur les minimales.")
        if littoral:
            env.append("L'influence maritime atténue les extrêmes et limite les inversions.")
        p3 = IMPL_TXT[topo]
        if littoral and topo in ("plaine", "plateau", "fond_vallee"):
            p3 = "Refroidissement nocturne limité par la mer ; exposition directe aux vents de secteur marin."
        notes = " ".join([p1, p2] + env + [p3])
        # facteurs
        sky = {"sommet": "très dégagé", "col": "dégagé", "plaine": "dégagé", "plateau": "dégagé",
               "versant": "partiellement masqué par la pente", "fond_vallee": "masqué par les reliefs (horizon relevé)",
               "cuvette_fermee": "horizon relevé tout autour", "cuvette_ouverte": "horizon relevé, ouvert d'un côté"}[topo]
        if aero:
            sky = "très dégagé"
        wind = {"sommet": "très exposé", "col": "très exposé (vent canalisé)", "plaine": "exposé",
                "plateau": "exposé", "versant": "moyen (brises de pente)", "fond_vallee": "abrité, vent canalisé par la vallée",
                "cuvette_fermee": "très abrité", "cuvette_ouverte": "abrité"}[topo]
        if littoral:
            wind = "exposé (vents marins)"
        a1 = info["amp1"]
        slope = "nulle" if a1 < 20 else "faible" if a1 < 60 else "moyenne" if a1 < 150 else "forte" if a1 < 300 else "très forte"
        if topo in ("cuvette_fermee", "fond_vallee", "cuvette_ouverte", "plaine") and a1 >= 60:
            slope = "faible au point, versants proches"
        if littoral and h < 200:
            snow = "rare"
        elif h < 300:
            snow = "rare" if dep not in MASSIFS["Vosges"] | {"67", "68", "57", "54", "55", "08", "25"} else "occasionnel"
        elif h < 700:
            snow = "occasionnel"
        elif h < 1200:
            snow = "régulier en hiver"
        elif h < 1800:
            snow = "durable (plusieurs mois)"
        else:
            snow = "durable et épais (novembre à mai)"
        if dep in MASSIFS["Corse"] and h < 600:
            snow = "rare"
        forest = "faible (aérodrome)" if aero else ("faible (urbain)" if urbain else None)
        open_t = False if topo == "cuvette_fermee" else True
        out.append({
            "id": s["id"], "nom": nom, "notes": notes, "category": cat, "size": size,
            "category_confiance": conf, "factor_sky": sky, "factor_wind": wind, "factor_slope": slope,
            "factor_snow_cover": snow, "factor_forest_cover": forest, "is_open_terrain": open_t,
            "commune": commune, "departement": dep,
            "sources": ["candidats_fr.json", "IGN RGE ALTI (data.geopf.fr, couronnes 1 et 3 km)"] + (["geo.api.gouv.fr"] if commune else []),
            "_topo": info,
        })
    # contrôle des règles category/size
    ok = {"Cat1": {None}, "Cat2": {None, "Gr"}, "Cat3": {"Gr", "Pt"}, "Cat4": {"Gr", "Pt"}, "Col": {None}, "Versants": {None}}
    assert all(o["size"] in ok[o["category"]] for o in out)
    OUT.write_text(json.dumps(out, ensure_ascii=False, indent=1), encoding="utf-8")
    from collections import Counter
    print(Counter(o["category"] for o in out), Counter(o["category_confiance"] for o in out))


if __name__ == "__main__":
    mode = sys.argv[1] if len(sys.argv) > 1 else "tout"
    if mode in ("enrichir", "tout"):
        enrichir()
    if mode in ("decrire", "tout"):
        decrire()
