# Protocole USB HID de l'ALIENTEK DP100

Document de référence pour `dp100-proto`. Il croise six sources communautaires avec le **désassemblage IL de la DLL officielle** `ATK-DP100DLL(x64)_2.0.dll`. La DLL est un assembly .NET, donc lisible tel quel avec `monodis` : c'est la source de vérité côté fabricant. Le tout a ensuite été **validé en lecture seule sur une vraie DP100** le 2026-10-17.

## Légende de confiance

| Marque | Signification |
|---|---|
| ✅ **HW** | Observé sur le matériel le 2026-10-17 (unité HW 1.4, firmware APP 1.5, boot 1.1). Log brut : [`captures/2026-10-17_readonly_fw1.5.log`](captures/2026-10-17_readonly_fw1.5.log) |
| 🟦 **DLL** | Lu dans le code IL de la DLL officielle. C'est ce que fait le logiciel du fabricant, mais ce n'est pas encore observé ici. |
| 🟨 **COMM** | Affirmé par une ou plusieurs sources communautaires, sans confirmation DLL ni HW |
| ❓ **SUPPOSÉ** | Hypothèse personnelle, à vérifier |

Sources (clonées dans `refs/`) :
[webdp100](https://github.com/scottbez1/webdp100) (RE de la DLL),
[open_dp100](https://github.com/lessu/open_dp100),
[pydp100](https://github.com/palzhj/pydp100) (contient de vraies captures),
[DP100-Lab](https://github.com/aIeXoid/DP100-Lab) (`dp100_proto`),
[DP100-PyQt5-GUI](https://github.com/ElluIFX/DP100-PyQt5-GUI) (wrapper de la DLL, et la DLL elle-même),
[weigu / Cynthion](https://www.weigu.lu/other_projects/usb/cynthion/index.html) (capture USB).

---

## 1. Couche USB

| Élément | Valeur | Confiance |
|---|---|---|
| VID:PID | `2E3C:AF01` | ✅ HW |
| Chaînes | Manufacturer `ALIENTEK`, Product `ATK-MDP100`, Serial `3F0F22964000` (sur cette unité) | ✅ HW |
| USB | 2.0 Full Speed, `bcdDevice 2.00`, bus-powered, 100 mA | ✅ HW |
| Interface | 1 interface HID (classe 3), sans boot protocol | ✅ HW |
| Endpoints | `0x81` IN interrupt 64 o, `bInterval 1` ; `0x01` OUT interrupt 64 o, `bInterval 1` | ✅ HW |
| Report descriptor (32 o) | `06 FF 00 09 01 A1 01 15 00 25 FF 75 08 95 40 09 01 81 02 95 40 09 01 91 02 95 01 09 01 B1 02 C0` | ✅ HW, identique à weigu |
| Rapports | Vendor page `0xFF00`. Input 64 o, Output 64 o, Feature 1 o (non utilisé). **Pas de Report ID.** | ✅ HW |
| Mode | La DP100 doit être en mode **USBD** (double appui sur ◀ pour basculer USBD/USBH) | 🟨 COMM (pydp100) |

### Écriture et lecture sous Linux (hidraw / hidapi)

- **Écriture** : 65 octets, soit `0x00` (report ID « aucun ») suivi de la trame paddée à 64 octets. C'est ce que fait la DLL (`HidReportData.SetReportID(0)`, puis copie de la trame à l'offset 1) 🟦. Sur Linux, une écriture de 64 octets sans préfixe marche aussi ✅ HW : le noyau ne retire le 1er octet que s'il vaut `0x00`, et une trame commence par `0xFB`. **On garde le préfixe `0x00`**, qui est la forme canonique attendue par hidapi.
- **Lecture** : 64 octets, sans report ID ✅ HW.
- **Contenu après le CRC** : l'appareil ne remet pas son buffer TX à zéro. Les octets après le CRC sont des **restes des réponses précédentes** ✅ HW (weigu l'avait vu aussi). Il faut toujours parser selon le champ `len`, jamais « jusqu'au premier zéro ».

---

## 2. Format de trame

```
offset  0      1      2      3      4 … 4+len-1   4+len   5+len
       ┌──────┬──────┬──────┬──────┬────────────┬───────┬───────┐
       │ addr │ func │ seq  │ len  │  payload   │ CRC lo│ CRC hi│   (puis padding jusqu'à 64)
       └──────┴──────┴──────┴──────┴────────────┴───────┴───────┘
```

| Champ | Description | Confiance |
|---|---|---|
| `addr` | `0xFB` hôte→appareil, `0xFA` appareil→hôte | ✅ HW, 🟦 DLL (`set_DeviceAddr(251)`) |
| `func` | Code de fonction (§4). La réponse **reprend** le code de la requête | ✅ HW |
| `seq` | Numéro de séquence. **L'appareil le renvoie tel quel** (testé avec `0x00`, `0x5A`, `0xFF`). La DLL envoie toujours 0, DP100-Lab l'incrémente | ✅ HW |
| `len` | Longueur du payload, 0 à 58 | ✅ HW |
| payload | Champs multi-octets en **little-endian** | ✅ HW, 🟦 DLL (`BitConverter`, x86) |
| CRC | CRC-16 sur `addr…payload` (en-tête compris), transmis **LSB d'abord** | ✅ HW |

### CRC

**CRC-16/MODBUS** : poly `0x8005` réfléchi (`0xA001`), init `0xFFFF`, refin et refout vrais, xorout `0x0000`. Valeur de contrôle sur `"123456789"` : `0x4B37`.
Confirmé par la DLL (`FrameEntity.CRC16` : `crc=0xFFFF`, `0xA001`, 8 décalages par octet) 🟦, par les cinq implémentations communautaires 🟨, et par toutes les trames RX de la session HW ✅.

Vecteurs de test (tous vus sur le fil) :

| Trame sans CRC | CRC (octets sur le fil) |
|---|---|
| `FB 10 00 00` | `30 C5` |
| `FB 30 00 00` | `31 0F` |
| `FB 40 00 00` | `30 D4` |
| `FB 35 00 01 80` | `CE 28` |
| `FA 35 00 01 01` (ACK OK) | `33 88` |
| `FA 30 00 01 00` (NAK) | `F2 84` |

### Corrélation requête / réponse, erreurs

- Il n'y a **aucune trame non sollicitée** (1 s d'écoute, rien reçu) ✅ HW. C'est strictement requête puis réponse.
- La latence aller-retour est de **0,9 à 2,2 ms** ✅ HW.
- Si le **CRC est faux**, l'appareil répond avec le même `func`, `len=1`, `payload=00` (NAK) ✅ HW.
- Les écritures sont acquittées par `len=1`, `payload=01` (OK) ou `00` (refus) 🟦 DLL (`activeFrame.Data[0] == 1`) et 🟨 (capture pydp100 `FA 35 00 01 01 33 88`).
- Conséquence pour le décodeur : sur une **lecture**, une réponse `len=1` / `00` est une **erreur**, pas des données.
- La DLL fait la correspondance **par `func` seulement**, avec des timeouts de ~50 ms en lecture et ~200 ms en écriture 🟦. Comme `seq` est renvoyé en écho, on peut faire mieux : faire correspondre `(func, seq)` ✅.

---

## 3. Séquence d'initialisation

**Aucune n'est requise** ✅ HW : `0x10` et `0x30` répondent dès l'ouverture du hidraw, sans handshake.
La DLL enchaîne : ouverture du chemin HID, puis `GetDevInfo` (0x10), puis `GetCurrentBasic` (0x35/0x80), puis un polling `GetBasicInfo` (0x30) 🟦.
Recommandation : à l'ouverture, envoyer 0x10, vérifier `run_area == 0xAA` (APP), et **refuser de piloter un appareil en bootloader**.

---

## 4. Codes de fonction

D'après l'enum `Frame_Func` de la DLL 🟦, identique dans webdp100 et open_dp100 :

| Code | Nom (DLL) | Usage | Requête | Réponse | Statut |
|---|---|---|---|---|---|
| `0x10` | `FRAME_DEVICE_INFO` | Identification | len 0 | 40 o (§5.1) | ✅ HW |
| `0x11` | `FRAME_FIRM_INFO` | Mise à jour firmware : en-tête | — | — | 🟦 **NE JAMAIS ENVOYER** |
| `0x12` | `FRAME_START_TRANS` | Mise à jour firmware : début | — | — | 🟦 **NE JAMAIS ENVOYER** |
| `0x13` | `FRAME_DATA_TRANS` | Mise à jour firmware : données (chiffrées) | — | — | 🟦 **NE JAMAIS ENVOYER** |
| `0x14` | `FRAME_END_TRANS` | Mise à jour firmware : fin | — | — | 🟦 **NE JAMAIS ENVOYER** |
| `0x15` | `FRAME_DEV_UPGRADE` | Passage en bootloader | — | — | 🟦 **NE JAMAIS ENVOYER** |
| `0x30` | `FRAME_BASIC_INFO` | Télémétrie | len 0 | 16 o (§5.2) | ✅ HW |
| `0x35` | `FRAME_BASIC_SET` | Consignes, sortie, presets | len 1 = lecture / len 10 = écriture | 10 o / ACK | lecture ✅ HW, écriture 🟦 |
| `0x40` | `FRAME_SYSTEM_INFO` | Réglages système | len 0 = lecture / len 8 = écriture | 8 o / ACK | lecture ✅ HW, écriture 🟦 |
| `0x45` | `FRAME_SYSTEM_SET` | Défini mais **jamais utilisé par la DLL** : l'écriture passe par 0x40, len 8 | — | — | 🟦 |
| `0x50` | `FRAME_SCAN_OUT` | Balayage V/I exécuté par le firmware | 12 o | ACK | 🟦, layout contesté (§6) |
| `0x55` | `FRAME_SERIAL_OUT` | Séquence de sortie exécutée par le firmware | 10 o | ACK | 🟦, layout contesté (§6) |
| `0x80` | `FRAM_DISCONNECT` | Déconnexion (payload `FF`) | len 1 | ACK ? | 🟦, non testé |

`dp100-proto` n'exposera **pas** les codes 0x11 à 0x15 : risque de brique.

---

## 5. Payloads

### 5.1 `0x10` DEVICE_INFO (réponse, 40 octets)

Struct DLL `DevInfo` (pack 1) 🟦, valeurs ✅ HW :

| Off | Type | Champ | Exemple HW | Interprétation |
|---|---|---|---|---|
| 0 | `u8[16]` | `dev_type` | `"ATK-DP100\0"` + `FF…` | ASCII terminé par NUL, paddé avec `0xFF` |
| 16 | `u16` | `hdw_ver` | 14 | version = valeur / 10, donc **1.4** (DLL : `"V" + (v/10).ToString("0.0")`) |
| 18 | `u16` | `app_ver` | 15 | firmware **1.5** |
| 20 | `u16` | `boot_ver` | 11 | bootloader 1.1 |
| 22 | `u16` | `run_area` | `0x00AA` | `0xAA` = APP, autre valeur = Boot 🟦 |
| 24 | `u8[12]` | `dev_sn` | `96E2990E0040753003A74D05` | 12 octets. La DLL affiche les **8 derniers caractères hex** (`03A74D05`). ❓ C'est probablement l'UID 96 bits du MCU STM32 |
| 36 | `u16` | `year` | 2026 | date (de build firmware ?) ❓ |
| 38 | `u8` | `month` | 9 | |
| 39 | `u8` | `day` | 28 | |

### 5.2 `0x30` BASIC_INFO (réponse, 16 octets)

Struct DLL `BasicInfo` (pack 1) 🟦 ; toutes les sources sont d'accord sur l'ordre :

| Off | Type | Champ | Unité | Exemple HW | Confiance |
|---|---|---|---|---|---|
| 0 | `u16` | `vin` | mV | 15223 | ✅ |
| 2 | `u16` | `vout` | mV (mesuré) | 4996 | ✅ |
| 4 | `u16` | `iout` | mA (mesuré) | 0 (à vide) | 🟨 unité mA d'après pydp100 et weigu (5 005 mV / 23 mA en charge). Non testé sous charge ici |
| 6 | `u16` | `vo_max` | mV, **Vout max atteignable avec le Vin actuel** | 14790 | ✅ valeur ; ❓ sémantique (§7) |
| 8 | `u16` | `temp1` | 0,1 °C | 284 (28,4 °C) | ✅ |
| 10 | `u16`/`i16` | `temp2` | 0,1 °C | 282 | ✅ ; signe : DLL `u16`, open_dp100 `i16`. On décode en `i16` |
| 12 | `u16` | `dc_5V` | mV, rail 5 V interne | 5083 | ✅ valeur ; ❓ sémantique |
| 14 | `u8` | `out_mode` | enum, voir ci-dessous | 1 | partiel |
| 15 | `u8` | `work_st` | enum, voir ci-dessous | 0 | partiel |

**`out_mode`** : c'est une divergence (§6.4). C'est aussi **le seul indicateur fiable de l'état réel de la sortie** (§5.3.1) : l'octet `state` du jeu actif peut mentir.

| Valeur | Interprétation retenue | Preuve |
|---|---|---|
| 1 | **CV** | ✅ HW : sortie ON, 5 V à vide. Capture weigu aussi : ON, 5 V / 23 mA |
| 2 | **OFF** | ✅ HW (2026-10-17) : après une vraie coupure, Vout tombe à 0 et `out_mode` passe à 2. Également vu dans deux captures tierces (pydp100, weigu). **DP100-Lab dit 2 = CC**, c'est faux |
| 0 | **CC** | ✅ HW : en régime établi, avec un ventilateur 12 V et une limite à 0,05 A, `out_mode = 0` pendant 3 s, I = 0,050–0,051 A, et Vout s'effondre de 5,9 à 3,5 V. Également vu au démarrage du moteur (0,498 A pour une limite à 0,5 A, pendant 334 ms). DP100-Lab dit 0 = OFF : c'est faux |

**`work_st`** : 0 = normal ✅ (toutes les captures). Pour le reste, seule DP100-Lab propose une table 🟨 non vérifiée : `1=OVP 2=OCP 3=OPP 4=OTP 5=REP (inversion) 6=UVP`. Le serveur rapportera la valeur brute en plus du libellé.

### 5.3 `0x35` BASIC_SET : consignes, sortie, presets

Payload de 10 octets. Struct DLL `Basic_Set` 🟦 (pas de `pack`, mais l'alignement naturel ne crée pas de padding) :

| Off | Type | Champ | Unité |
|---|---|---|---|
| 0 | `u8` | `index` : **bits 7..4 = drapeaux d'opération, bits 3..0 = n° de preset 0-9** | |
| 1 | `u8` | `state` : sortie 0 = OFF, 1 = ON | |
| 2 | `u16` | `vo_set` | mV |
| 4 | `u16` | `io_set` | mA |
| 6 | `u16` | `ovp_set` | mV |
| 8 | `u16` | `ocp_set` | mA |

**Lecture** (requête `len = 1`, payload = `[index]`) :

| `index` envoyé | Effet | Source DLL | Statut |
|---|---|---|---|
| `0x80` (\| n ?) | Lit le **jeu actif**, c'est-à-dire les consignes courantes et l'état de la sortie | `GetCurrentBasic` : `CurrentBasicIndex \| 0x80`, avec `CurrentBasicIndex` à 0 par défaut | ✅ HW |
| `0x00`–`0x09` | Lit le **preset n** | `GetGroupInfo(index)` envoie l'index brut | ✅ HW, les 10 presets ont été lus |

Dans la réponse, l'octet `index` revient sans drapeau (`0x80` donne `0x00`). ✅ HW.
❓ On ne sait pas si `index` vaut « le n° du preset actif » ou toujours 0 : ici l'actif est P0, donc les deux sont indiscernables.
✅ HW : **`0x80` (jeu actif) et `0x00` (preset P0 stocké) sont deux enregistrements distincts.** Après une écriture `0x20` à 3,3 V, `0x80` relit 3,3 V et `0x00` relit toujours 5 V. Le preset stocké P0 garde aussi `state=1` alors que le jeu actif est à `state=0`.

**Écriture** (requête `len = 10`, réponse ACK `len 1`) : `0x20` est testé sur le matériel, sortie OFF (§5.3.1). `0x40` et `0x80` ne le sont pas encore.

| Drapeau | Méthode(s) DLL | Effet | `state` envoyé | Statut |
|---|---|---|---|---|
| `0x20 \| n` | `SetBasic`, `OpenOut` (state 1), `CloseOut` (state 0), `SetBasicInfo` | **Applique V/I/état au jeu actif** (n = preset actif) ; ne modifie pas le preset stocké | 0 ou 1 | ✅ HW : V/I, allumage et coupure, à condition de respecter la règle de timing de §5.3.1 |
| `0x40 \| n` | `SetGroupInfo`, `SetBasic_Write` | **Enregistre le preset n en flash** sans toucher la sortie | 0 | 🟦 ; la DLL **refuse n = 0** (`if index == 0 return false`) |
| `0x80 \| n` | `UseGroup`, `SetBasic_CallOut` | **Rappelle le preset n** : il devient actif | 0 | 🟦 ; la DLL **refuse n = 0**. Le payload V/I semble ignoré : ElluIFX envoie des zéros ❓ |
| `0xA0 \| n` | — | « modifier et activer » | — | 🟨 open_dp100 seul, **absent de la DLL** : ne pas utiliser |

Points d'attention pour l'écriture `0x20` :

- **OVP et OCP** : `OpenOut` et `CloseOut` de la DLL envoient `ovp_set = ocp_set = 0`. On ne sait pas si le firmware ignore ces champs en `0x20` ou s'il les applique ❓. pydp100, webdp100 et DP100-Lab renvoient des valeurs réelles. **Notre choix : toujours relire le jeu actif (`0x80`) et renvoyer les OVP/OCP lus**, sûr dans les deux cas.
- **Fermeture en deux temps** : ElluIFX, pour couper, envoie d'abord `CloseOut` avec V = I = 0, attend 40 ms, puis renvoie `CloseOut` avec les vraies consignes. ✅ HW : ce n'est pas nécessaire en soi. Ce qui fait marcher cette séquence, c'est la **pause** entre deux requêtes ; voir §5.3.1.
- **Valeurs des presets sur cette unité** (pour référence et tests) : P0 5 V / 0,5 A (actif, ON) ; P1 à P9 de 2 V / 1 A à 10 V / 5 A par pas de 1 V / 0,5 A. OVP 30 500 mV et OCP 5 050 mA partout, soit les maxima.

#### 5.3.1 Règle de timing des écritures : une lecture `0x35` trop proche annule l'écriture (fw 1.5)

Logs :
- [`captures/2026-10-17_writes_output_off.log`](captures/2026-10-17_writes_output_off.log) et [`captures/2026-10-17_output_on_tests.log`](captures/2026-10-17_output_on_tests.log), à vide ;
- [`captures/2026-10-17_fan_load.log`](captures/2026-10-17_fan_load.log), avec un ventilateur 12 V / 0,3 A ; les CSV sont dans `captures/fan_12v/`.

**Constat ✅ HW.** Le firmware acquitte une écriture `0x20` (`01`) et la mémorise tout de suite : une relecture `0x80` renvoie les nouvelles valeurs. Il ne l'applique à la sortie qu'un peu plus tard, vraisemblablement dans sa boucle principale. **Si une lecture `0x35` arrive dans cet intervalle, l'écriture n'est jamais appliquée**, alors que l'appareil continue d'afficher et de relire la nouvelle consigne.

| Écriture `0x20` (sortie ON, ventilateur) | Requête suivante | Résultat 1 s plus tard |
|---|---|---|
| 12 → 11 V | `0x35/0x80`, envoyée tout de suite (~2 ms) | **non appliquée** (3 cas sur 3) |
| 12 → 11 V | `0x35/0x80` à +5 ms | **non appliquée** |
| 12 → 11 V | `0x35/0x80` à +20 ms, +50 ms, +200 ms | appliquée |
| 11 → 12 V | `0x30` (télémétrie), envoyée tout de suite | appliquée |
| OFF, V/I inchangés | `0x35/0x80`, envoyée tout de suite | **non appliquée** : `state=0` relu, mais la sortie reste ON |
| OFF, V/I inchangés | aucune pendant 300 ms | appliquée |
| ON | `0x35/0x80`, envoyée tout de suite | appliquée (2 cas sur 2 ; l'allumage semble traité de façon synchrone ❓) |

La fenêtre fait donc **entre 5 et 20 ms**. Cette race explique les deux symptômes rencontrés :
- **La coupure « ignorée »** (premier test, à vide) : mon code relisait le jeu actif juste après l'écriture.
  - La séquence ElluIFX (V = I = 0, pause de 40 ms, consignes) marchait **grâce à la pause**, pas grâce au V = 0.
  - J'en avais d'abord conclu que « OFF n'est appliqué que si V/I changent » : **c'est faux**, le test ci-dessus avec V/I inchangés et sans lecture coupe bien.
- **La rampe figée** (ventilateur) : après 12 → 5 V, les paliers 6 à 12 V étaient acquittés et relus, mais Vout restait à 4,99 V.

**Autres mesures ✅ HW :**
- `0x20` ne modifie pas le preset stocké : `0x80` et `0x00` deviennent différents.
- L'allumage est appliqué que V/I changent ou non.
- Temps d'établissement : ON en 100 à 200 ms à vide, et 5 → 12 V en moins de 0,5 s avec le ventilateur. OFF : sous 1 V en environ 50 ms à vide. Avec le ventilateur, Vout décroît lentement après la coupure, parce que le moteur continue de tourner et renvoie une tension ; `out_mode` passe pourtant tout de suite à 2.
- Démarrage du ventilateur à 12 V avec une limite à 0,5 A : 334 ms en CC à 0,498 A, puis CV. En régime établi : 0,287–0,323 A.

**Conséquences pour l'implémentation (`dp100`) :**
- **Garde post-écriture** dans l'actor : aucune requête autre que `0x30` pendant **100 ms** après une écriture, soit 5 fois la fenêtre observée.
- L'état réel de la sortie se lit dans la **télémétrie** (`out_mode ≠ 2`), jamais dans l'octet `state`, qui reflète ce qui a été écrit, pas ce qui est appliqué.
- **Vérification après écriture** :
  - l'état ON/OFF doit être atteint en 800 ms au maximum ;
  - si la sortie est ON, Vout doit être à ±max(150 mV, 2 %) de la consigne en CV (ou la sortie en CC), en 1,5 s au maximum.
  - Sinon, la même trame est renvoyée **une fois** (une écriture est idempotente). Si ça ne suffit pas :
    - sortie toujours ON alors qu'OFF était demandé : erreur bloquante `SAFETY` ;
    - Vout au-dessus de la consigne : erreur bloquante `SAFETY` ;
    - sortie OFF au lieu de ON, ou Vout sous la consigne : avertissement.
- Coupure en **une seule** écriture : la double écriture ElluIFX n'est plus utilisée.

### 5.4 `0x40` SYSTEM_INFO : réglages système

Lecture : requête `len = 0`, réponse de 8 octets ✅ HW (identique, octet pour octet, à la capture pydp100).
Écriture : requête `len = 8` avec le même layout, sur **`func 0x40`** et non 0x45, réponse ACK 🟦. Non testé.

| Off | Type | Champ | Unité / plage | Exemple HW |
|---|---|---|---|---|
| 0 | `u16` | `otp` | °C, 50–80 (doc ElluIFX) | 80 |
| 2 | `u16` | `opp` | **0,1 W**, ≤ 1050 (105 W) | 1050 |
| 4 | `u8` | `blk_lev` (rétroéclairage) | 0–4 | 2 |
| 5 | `u8` | `vol_lev` (volume du bip) | 0–4 | 2 |
| 6 | `u8` | `en_rep` : protection contre l'inversion de polarité | 0/1 | 1 |
| 7 | `u8` | `en_auto_out` : sortie active au démarrage | 0/1 | 0 |

Ancien firmware : si la réponse ne fait que **6 octets**, la DLL lit `otp, opp, blk, vol` et met `en_rep = en_auto_out = 0` 🟦.

Les protections OVP et OCP **ne sont pas** ici : elles sont par preset, dans 0x35. OTP et OPP sont globales.

---

## 6. Divergences entre les sources

| # | Sujet | Sources en désaccord | Tranché par | Retenu |
|---|---|---|---|---|
| 6.1 | **Layout de SYSTEM_INFO** | open_dp100 et pydp100 : `blk u8, opp u16, opt u16, vol u8` (6 o) | DLL `GetSystem` + HW + capture pydp100 elle-même | `otp u16, opp u16, blk, vol, en_rep, en_auto_out` (8 o). Le décodage open_dp100/pydp100 est **faux** : sur leur propre capture, il donne blk = 80 |
| 6.2 | Écriture des réglages système | open_dp100 : 0x45 « inconnu » | DLL `SetSystem` | 0x40, len 8 |
| 6.3 | Drapeaux de `index` en 0x35 | open_dp100 : `0x20` modifier, `0x80` activer, `0xA0` les deux. weigu : `0xA_` activer, `0x2_` on/off, `0x4_` écrire. DP100-Lab : `0x20` sortie, `0x40` sauver, `0x80` activer | DLL | `0x20` appliquer à la sortie, `0x40` sauver en flash, `0x80` rappeler/activer, `0x80` en len 1 = lire l'actif. **`0xA_` de weigu est faux, `0xA0` d'open_dp100 n'existe pas dans la DLL** |
| 6.4 | `out_mode` | DP100-Lab : `0 OFF, 1 CV, 2 CC` | Captures pydp100 et weigu (OFF donne 2) + HW (CV donne 1) | `1 CV, 2 OFF, 0 CC ❓` |
| 6.5 | Numéro de série | DP100-Lab : `dev_sn[0..8]` en hex. pydp100 : `dev_sn[0..11]`. DLL : 8 derniers caractères hex des 12 octets | DLL | Exposer les 12 octets complets en hex et l'USB `iSerial`. Afficher aussi le format DLL |
| 6.6 | Layout de SCAN_OUT (0x50) | open_dp100 et DP100-Lab : `on_off, on_time, out_val, scan_mode, start, end, step` | DLL struct `Scan_Out` (pack 1) | DLL : `scan_mode u8, on_off u8, out_val, start, end, step, on_time` ❓ non testé. Par ailleurs DP100-Lab dit `mode 0 = courant`, la DLL dit `ScanMode_V = 0, ScanMode_I = 1` |
| 6.7 | Layout de SERIAL_OUT (0x55) | open_dp100 et DP100-Lab : `on_off, on_time, start, end, vi, vo, cycles` | DLL struct `Serial_Out` | DLL : `on_off, cycle_times, ser_vo, ser_vi, on_time, ser_start, ser_end` ❓ non testé |
| 6.8 | `set_protection` de DP100-Lab | Envoie `0x40` avec V = I = 0 | Lecture du code | **Bug de DP100-Lab** : cela écrit le preset 0 avec 0 V / 0 A. À ne pas reproduire |
| 6.9 | OVP/OCP dans `0x20` | La DLL envoie 0, les autres sources de vraies valeurs | — | Renvoyer les valeurs relues (§5.3) |
| 6.10 | Report ID à l'écriture | pydp100 : « sous Windows le 1er octet disparaît » | DLL + HW | Toujours préfixer `0x00`, qui est le report ID |
| 6.11 | Signe de `temp2` | DLL `u16`, open_dp100 et DP100-Lab `i16` | — | `i16`, sans impact en pratique |
| 6.12 | Coupure de la sortie | La DLL fait un seul `CloseOut` ; ElluIFX ajoute une 2e écriture avec V = I = 0, 40 ms plus tard | HW (§5.3.1) | Une seule écriture suffit, **à condition de ne rien lire en `0x35` dans les ~20 ms qui suivent**. La séquence ElluIFX contourne sans le savoir cette race |

Les fonctions 0x50 et 0x55 (balayage et séquence côté firmware) **ne seront pas utilisées** : `ramp_voltage` et `power_cycle` seront pilotés côté hôte via 0x35/0x20, ce qui garde la mesure à chaque palier et le contrôle des limites.

---

## 7. Marge du régulateur (Vout ≤ Vin − marge)

L'appareil publie déjà sa limite : **`vo_max`**, dans BASIC_INFO. Points de mesure :

| Source | Vin (mV) | vo_max (mV) | Vin − vo_max | vo_max / Vin |
|---|---|---|---|---|
| HW 2026-10-17 | 15 223 | 14 790 | 433 | 0,972 |
| weigu | 20 168 | 19 550 | 618 | 0,969 |
| pydp100 | 5 221 | 5 050 | 171 | 0,967 |

Donc `vo_max ≈ 0,97 × Vin` ❓ (formule inférée, pas lue dans le firmware). pydp100 refuse déjà toute consigne `> vo_max`.

**Règle proposée pour `set_output`** :

```
V_set ≤ min( vo_max, Vin − dropout )
dropout = 500 mV par défaut, configurable
```

`vo_max` vient du firmware ; le terme `Vin − 500 mV` ajoute une marge si `vo_max` se révélait optimiste ou si Vin s'effondre sous charge. Pour Vin ≥ ~16,7 V, c'est `vo_max` qui limite ; en dessous, c'est le dropout fixe. À 15,2 V, cela donne V_set ≤ 14,72 V. Le tout s'ajoute aux limites `--max-voltage` / `--max-current`, appliquées avant tout envoi.

---

## 8. Plusieurs DP100 et sélection

- La chaîne USB **`iSerial`** (`3F0F22964000` ici) est lisible **à l'énumération**, sans ouvrir l'appareil (hidapi `serial_number`, et `HID_UNIQ` côté udev) ✅. On l'utilise comme clé de sélection.
- `dev_sn` (0x10, 12 octets) est différent de `iSerial` ; ❓ le lien entre les deux est inconnu.
- La DLL sait ouvrir par numéro de série (`OpenDevice(vid, pid, pvn, serialNumber)`) 🟦, ce qui confirme que c'est prévu.
- ❓ Unicité de `iSerial` entre plusieurs unités : non vérifiable avec une seule DP100. Repli possible : le chemin hidraw ou USB.

---

## 9. Reste à valider sur le matériel (demandera ton accord)

1. ~~Écriture `0x20` : coupure, allumage, consignes, temps d'établissement~~ ✅ (§5.3.1). L'effet de `ovp/ocp = 0` en `0x20` n'est pas testé, et on ne l'utilise pas.
2. ~~`out_mode = 0` (CC) en régime établi~~ ✅ (ventilateur, limite 0,05 A). Reste : les codes `work_st` hors 0, qui demandent de déclencher volontairement une protection.
7. Race lecture-après-écriture (§5.3.1) : vérifier si elle touche aussi `0x40 | n`, `0x80 | n` et l'écriture `0x40`. Par prudence, la garde de 100 ms s'applique déjà à toutes les écritures.
3. `0x40 | n` et `0x80 | n` (presets), sur un preset non actif.
4. Écriture de 0x40 (réglages système) : au minimum un aller-retour sans changement de valeur.
5. ~~Débit de polling soutenable~~ ✅ HW : 100 Hz soutenus en 0x30 (201 échantillons sur 2 s, 0 manqué) via `dp100` / hidapi.
6. ~~Unité de `iout` sous charge réelle~~ ✅ mA (ventilateur 12 V / 0,3 A : 0,287–0,323 A).
