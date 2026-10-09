# Descriptions des stations FR (`descriptions_fr.json`)

Généré par `scripts/stations_fr/decrire_stations.py` (`enrichir` puis `decrire`, reprenable via `enrichissement_cache.json`). Infoclimat n'a pas été interrogé.

## Méthode
1. **Commune / département** : geo.api.gouv.fr (commune contenant le point). 1151/1153 trouvées (2 hors territoire communal, en mer ou à la frontière).
2. **Topographie** : altitudes IGN RGE ALTI (data.geopf.fr) au point et sur 2 couronnes de 8 directions (1 km et 3 km). Indicateurs : écart au relief moyen (TPI 1 km / 3 km), dénivelé, nombre de directions plus hautes, points en mer (littoral).
3. **Classification par règles**, dans l'ordre :
   - `Col` : 2 directions opposées plus hautes de plus de 40 m et les 2 perpendiculaires plus basses de plus de 30 m ;
   - `Cat3` / `Cat4` (≥ 1400 m) : 8 directions plus hautes à 1 km (et à 3 km pour une confiance moyenne) ; `Gr` si le dénivelé à 3 km dépasse 150 m, sinon `Pt` ;
   - `Versants` : pente marquée (dénivelé > 120 m à 1 km), ou sommet/crête (TPI > +40 m, confiance basse car l'enum n'a pas de catégorie « sommet ») ;
   - `Cat2` : ≥ 6 directions plus hautes et TPI 3 km < −30 m ; `Gr` si dénivelé à 3 km > 250 m ;
   - `Cat1` : le reste (fond de vallée, plaine, plateau).
4. **Textes** : modèles de phrases alimentés par la classification (situation, topographie chiffrée, environnement urbain/aérodrome/littoral, conséquences pour la mesure). Facteurs déduits de la même façon (enneigement selon l'altitude et la région). `factor_forest_cover` n'est renseigné que pour les aérodromes et les villes : ailleurs il est à `null` (aucune donnée d'occupation du sol utilisée).
5. Règles category/size vérifiées par assertion. Le champ `_topo` (indicateurs bruts) est à ignorer à l'import.

## Répartition
| Catégorie | Taille | Nombre |
|---|---|---|
| Cat1 | – | 815 |
| Versants | – | 183 |
| Cat2 | Gr | 73 |
| Cat2 | – | 45 |
| Cat3 | Gr | 17 |
| Cat3 | Pt | 17 |
| Col | – | 3 |
| Cat4 | – | 0 |

Confiance : haute 369, moyenne 365, **basse 419 à relire** (surtout des Cat1 « plateau / relief modéré » et des sommets classés Versants).

## Limites connues
- La maille 1 km / 3 km ne détecte pas les micro-cuvettes (< 500 m) ni les dolines : des Cat2/Cat3 réelles peuvent être classées Cat1.
- Aucune Cat4 détectée : les cuvettes d'altitude ouvertes sortent en Cat2, à revoir à la main.
- Les textes sont génériques (pas de nom de vallée ni de massif précis, hors grande région) ; les descriptions des stations remarquables gagneront à être réécrites à la main.
