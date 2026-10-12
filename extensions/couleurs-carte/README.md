# Extension GWS : Couleurs de la carte

Extension native permettant de régler finement le rendu des fonds de carte (photos aériennes BD ORTHO et cartes IGN TOP25).

## Réglages proposés

- **Luminosité** (-100 à +100, 0 = neutre) : assombrit ou éclaircit le fond de carte.
- **Saturation** (-100 à +100, 0 = neutre) : de niveaux de gris purs à couleurs saturées.
- **Netteté** (-100 à +100, 0 = neutre) : filtre de convolution SVG `feConvolveMatrix` pour rehausser les détails ou adoucir le grain.
- **Bouton Réinitialiser** : remet instantanément les trois valeurs à 0.

## Permissions

Aucune (`permissions: []`). L'extension n'a besoin ni d'écrire ou lire dans SQLite, ni d'accéder au réseau. Les réglages sont conservés dans un fichier `reglages.json` au sein du `data_dir` attribué par l'hôte.

## Compilation

```bash
cargo build --release
```

La bibliothèque dynamique est produite dans `target/release/couleurs_carte.dll`.

## Empaquetage

Pour générer l'archive `.zip` signée pour publication :

```bash
python scripts/extensions/publier_extension.py paquet extensions/couleurs-carte
```
