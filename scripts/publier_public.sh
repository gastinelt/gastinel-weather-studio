#!/usr/bin/env bash
# Publie une version dans le depot PUBLIC (gastinelt/gastinel-weather-studio).
#
# Le depot public n'a pas l'historique du depot prive : chaque version y arrive
# en UN commit, copie exacte de l'etat de <ref> moins les chemins listes dans
# scripts/public_exclus.txt (docs d'infra, rapports internes).
#
# Usage (depuis la racine du depot, Git Bash) :
#   scripts/publier_public.sh [ref]        # ref par defaut : master
#   scripts/publier_public.sh master --oui # sans la question finale
set -euo pipefail

REF="${1:-master}"
CONFIRME="${2:-}"
PUBLIC_URL="https://github.com/gastinelt/gastinel-weather-studio.git"
RACINE="$(git rev-parse --show-toplevel)"
EXCLUS="$RACINE/scripts/public_exclus.txt"
TRAVAIL="$(mktemp -d)"
trap 'rm -rf "$TRAVAIL"' EXIT

VERSION="$(git -C "$RACINE" show "$REF:src-tauri/Cargo.toml" | sed -n 's/^version = "\(.*\)"/\1/p' | head -1)"
[ -n "$VERSION" ] || { echo "Version introuvable dans $REF:src-tauri/Cargo.toml" >&2; exit 1; }
echo "Publication de $REF (version $VERSION)"

# 1. Copie du depot public existant (pour garder ses commits precedents)
git clone -q "$PUBLIC_URL" "$TRAVAIL/pub"
cd "$TRAVAIL/pub"
git rm -rq --ignore-unmatch . >/dev/null

# 2. Etat exact de <ref>, moins les exclusions
git -C "$RACINE" archive "$REF" | tar -x -C .
while IFS= read -r motif; do
  case "$motif" in ''|\#*) continue ;; esac
  # shellcheck disable=SC2086
  rm -rf -- $motif
done < "$EXCLUS"

# 3. Garde-fou : motifs de secrets (les valeurs fictives de test sont tolerees)
if grep -rnIE '(gsk_[A-Za-z0-9]{20,}|ghp_[A-Za-z0-9]{20,}|github[_]pat_|AKIA[0-9A-Z]{16}|BEGIN [A-Z ]*PRIVATE KEY)' . \
     --exclude-dir=.git | grep -v 'UNECLEFICTIVE'; then
  echo "ARRET : motif de secret detecte ci-dessus, rien n'est publie." >&2
  exit 1
fi

git add -A
if git diff --cached --quiet; then
  echo "Rien de nouveau par rapport au depot public."; exit 0
fi
# Adresse publique de l auteur pour les publications.
git -c user.name="Theo Gastinel" \
    -c user.email="studio@gastinel.me" \
    commit -q -m "Gastinel Weather Studio $VERSION"
git --no-pager show --stat --oneline HEAD | tail -5

if [ "$CONFIRME" != "--oui" ]; then
  read -r -p "Pousser vers le depot public ? (o/N) " r
  [ "$r" = "o" ] || { echo "Annule."; exit 0; }
fi
git push -q origin HEAD:main
echo "Publie : Gastinel Weather Studio $VERSION"
