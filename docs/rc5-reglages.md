# Réglages du RC-5 : ElitechLog, protocole, et ce que GWS en fait

Rédigé le 12/10/2026, mis à jour pour GWS 0.6.0. Sources :
- captures d'ElitechLog fournies par Théo (RC-5, firmware V1.2) ;
- code d'ElitechLog 8.0.5 (`DL.exe`, .NET), lu par rétro-ingénierie ;
- bibliothèque non officielle `civic/elitech-datareader` (`elitech/msg.py`) ;
- manuels Elitech du RC-5 et du RC-5+ ;
- code de GWS (`src-tauri/src/elitech_serial.rs`, `src/rc5.js`).

Légende : ✅ fait · 👁️ lu et affiché · ❌ absent · ⛔ exclu volontairement.
**Rien de ce qui est marqué 0.6.0 n'a encore été testé sur un logger réel.**

---

## 1. Les réglages d'ElitechLog (non grisés) et leur place dans GWS

| Réglage ElitechLog | Octet du protocole | GWS 0.5.6 | GWS 0.6.0 |
|---|---|---|---|
| Intervalle d'enregistrement (H / M / S) | 5-7 | 👁️ | ✅ modifiable (10 s à 24 h) |
| Délai de démarrage (pas de 0,5 h) | 148 | ❌ ; remis à 0 par « Effacer » | ✅ modifiable (0 à 6 h), affiché |
| Bouton d'arrêt | 27 | ❌ ; passé à « Activé » par « Effacer » | ✅ modifiable, affiché |
| Limite supérieure / inférieure | 8-11 | 👁️ | ✅ modifiables (−40 à 90 °C, écart ≥ 0,5 °C) |
| Description du voyage | 38-137 | 👁️ | ✅ modifiable, **sans effacer** |
| Unité de l'écran | 151 | lue, pas affichée | ✅ modifiable, affichée |
| Adresse | 1 | lue, pas affichée | ✅ modifiable (1 à 254), affichée |

Réglages grisés dans ElitechLog pour ce modèle, mais portés par le protocole :

| Réglage | Octet | GWS 0.6.0 |
|---|---|---|
| Tonalité des boutons | 149 | ✅ dans « Réglages avancés » |
| Alarme sonore (0, 3, 10) | 150 | ✅ dans « Réglages avancés » |
| Correction de température (±5 °C) | 152 | ✅ dans « Réglages avancés », avec un avertissement : elle modifie les mesures à la source |
| Numéro de série | 138-147 | ✅ dans « Réglages avancés », mot CONFIRMER exigé |

Sans objet sur le RC-5 : humidité, temps d'affichage de l'écran, type de capteur, mode de
stockage, affichage min/max, intervalle d'alarme sonore, « raccourcir l'intervalle ».

Lus et affichés depuis la 0.6.0 :
- la **version du firmware** (octet 4, « V1.2 ») ;
- la **pile** (octet 28, en %).

On les croyait absentes du protocole, mais ElitechLog les lit à ces offsets.

## 2. Les actions

| Action | ElitechLog | GWS 0.6.0 |
|---|---|---|
| Lire, exporter | ✅ | ✅ « Relire », « Sauvegarder », export CSV / TXT |
| Remettre à l'heure sans effacer | ❌ (seulement via la réinitialisation) | ✅ « Remettre à l'heure » |
| Réinitialisation rapide | ✅ efface et remet à l'heure, garde les réglages | ✅ idem ; la remise à l'heure peut être décochée |
| Enregistrer les paramètres | ✅ efface la mémoire | ✅ « Modifier les réglages » ; le bilan dit avant l'envoi si la mémoire sera effacée |
| Arrêter | ✅ | ✅ avec confirmation |
| Démarrer | ❌ pour le RC-5 | Assistant : dit de maintenir ▶ environ 5 s, puis relit l'état pour confirmer |
| Changer le modèle | ❌ | ⛔ commande `04` connue mais exclue : le logger ne serait plus reconnu |

### Pourquoi il n'y a pas de démarrage par logiciel

Le démarrage logiciel n'est **pas possible** sur le RC-5. Les preuves :
- ElitechLog 8.0.5 n'envoie aucune commande de démarrage aux appareils de la famille
  CH340 (RC-5, RC-4…). Son « Démarrer » n'existe que pour les modèles USB HID (RC-5+, …),
  qui démarrent tout seuls au débranchement.
- Le manuel du RC-5 ne connaît que l'appui long sur ▶.
- Les sondages GWS des codes `03`, `04`, `06`, `0A`, `0C` et `0D` n'ont rien démarré.

Le « Démarrer » vu dans ElitechLog concerne donc un autre modèle.

## 3. Le protocole

Le protocole n'est pas documenté par Elitech. Ce qui suit vient d'`elitech-datareader`,
d'ElitechLog 8.0.5 et des sondages faits pour GWS.

| Commande | Trame | Effet | GWS |
|---|---|---|---|
| Init | `CC 00 0A 00 D6` | ouvre la session | lecture |
| DevInfo | `CC 00 06 00 D2` → 160 octets | identité et réglages | lecture, relecture après chaque écriture |
| DataHeader / DataBody | `33 st 01 00` / `33 st 02 page` | mesures | lecture |
| ParamPut | `33 st 05 00` + 20 octets | écrit les réglages, **efface la mémoire**, repasse en « jamais démarré » | réglages, réinitialisation |
| ClockSet | `33 st 07 00` + date sur 7 octets | remet l'horloge à l'heure, sans effacer | remise à l'heure |
| Stop | `33 st 08 00` | arrête sans effacer | arrêt |
| UserInfo | `33 st 09 00` + 100 octets | description, sans effacer | réglages |
| DevNum | `33 st 0B 00` + 10 octets | numéro de série | réglages avancés |
| SetDeviceModel | `33 st 04 00` + modèle | change le modèle annoncé | ⛔ exclu |

Les champs de ParamPut, dans l'ordre :
1. intervalle (h, min, s) ;
2. limite haute, puis limite basse ;
3. nouvelle adresse ;
4. bouton d'arrêt (`13` = activé, `31` = désactivé) ;
5. délai (heures × 16, + 1 pour une demi-heure) ;
6. tonalité (`13` / `31`) ;
7. alarme (`00`, `03`, `0A`) ;
8. unité (`31` = °C, `13` = °F) ;
9. correction (dixièmes de °C) ;
10. humidité (sans objet).

Depuis la 0.6.0, GWS **recopie les octets bruts lus** pour tout ce qui ne change pas :
une réinitialisation ne modifie plus en douce le bouton d'arrêt, le délai, la tonalité ou
l'alarme.

Chaque écriture se fait en une seule séquence, toujours dans le même ordre :
1. lecture de DevInfo et validation de **tout** ce qui est demandé (rien n'est envoyé si un
   réglage est refusé) ;
2. arrêt de l'enregistrement si des paramètres doivent être écrits ;
3. heure, description, numéro de série, puis paramètres, chacun dans sa propre session ;
4. nouvelle lecture de DevInfo, puis comparaison. Chaque écart est dit en clair.

## 4. Points à vérifier sur le logger réel

1. La remise à l'heure pendant un enregistrement : l'horloge change-t-elle sans effacer, et
   sans changer l'instant de départ annoncé ?
2. La description et le numéro de série : écrits sans effacer la mémoire ?
3. Le délai de démarrage : le ▶ clignote-t-il, et l'état passe-t-il à « démarrage différé » ?
4. La pile : la valeur en % est-elle cohérente avec l'icône de l'écran ?
5. L'assistant de démarrage : l'état passe-t-il à « enregistrement en cours » après un appui
   long sur ▶ **câble branché** ? (Le message conseille de débrancher si rien ne se passe.)
