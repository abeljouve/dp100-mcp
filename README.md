# dp100-mcp

Serveur MCP en Rust pour l'alimentation de labo **ALIENTEK DP100** (30 V / 5 A / 100 W), avec une CLI de debug. Il sert à piloter la DP100 depuis des agents (Claude Code, OpenCode) dans des bancs de test : power-cycling d'un DUT, mesure de consommation, rampes de tension.

Le protocole USB HID (propriétaire, reversé) est documenté dans [`docs/PROTOCOL.md`](docs/PROTOCOL.md), avec le niveau de confiance de chaque champ.

## Workspace

| Crate | Rôle |
|---|---|
| `dp100-proto` | Codec sans I/O : trames, CRC-16/MODBUS, types forts (`Millivolts`, `Milliamps`…). Testé avec des trames réelles |
| `dp100` | Couche device : hidapi (backend hidraw `linux-native`), un thread *actor* par appareil, timeouts, reconnexion automatique, sélection par numéro de série, limites de sécurité, mesures, rampes, simulateur |
| `dp100-mcp` | Serveur MCP (SDK officiel `rmcp` 3.5) : stdio par défaut, Streamable HTTP avec `--http` |
| `dp100ctl` | CLI de debug : `list`, `status`, `set`, `on`, `off`, `cycle`, `watch`, `presets`, `settings` |

## Sécurité

- **Limites côté serveur**, appliquées avant tout envoi : `--max-voltage`, `--max-current` ou `[limits]` dans un fichier TOML. Une demande hors limite est **refusée** avec un message explicite, jamais clampée.
- **Marge du régulateur** : `V_set ≤ min(vo_max, Vin − 500 mV)`. `vo_max` est la tension max que le firmware annonce pour le Vin courant (≈ 0,97 × Vin). La marge se règle avec `--vin-dropout-mv`. Voir PROTOCOL.md §7.
- **OVP / OCP** : une consigne ≥ OVP ou > OCP du jeu actif est refusée, puisqu'elle déclencherait la protection.
- **Pas d'activation implicite** : `set_output` sans `enable` ne touche pas l'état ON/OFF. `output_off` n'est jamais refusé. `power_cycle` refuse de partir d'une sortie éteinte sauf avec `allow_from_off: true`. `apply_preset` recoupe la sortie si le rappel l'a allumée.
- **Relecture** après chaque écriture : les résultats donnent ce que l'appareil a réellement appliqué, avec des `warnings` en cas d'écart.
- **État réel de la sortie lu dans la télémétrie** (`out_mode`), pas dans l'octet `state`. Le firmware 1.5 abandonne silencieusement une écriture acquittée si une lecture `0x35` la suit de moins de ~20 ms (PROTOCOL.md §5.3.1). L'actor impose donc 100 ms de garde après chaque écriture. Chaque écriture est ensuite vérifiée sur la télémétrie (ON/OFF, puis Vout ≈ consigne en CV), puis relancée une fois si besoin. Une sortie qui reste ON, ou un Vout au-dessus de la consigne, produit une erreur `SAFETY: …`.
- **Écritures jamais rejouées** : si l'ACK d'une écriture est perdu, l'erreur dit que l'état est inconnu. Seules les lectures sont relancées.
- **Arrêt** (SIGINT/SIGTERM, ou déconnexion du client en stdio) : `--on-exit keep` (défaut) ou `off`.
- Les codes de mise à jour firmware (0x11–0x15) ne sont même pas exposés par `dp100-proto`.

## Installation

### Déploiement recommandé : un service partagé

Un **seul** processus doit piloter une DP100. Si deux processus y accèdent en même temps, une lecture de l'un peut annuler une écriture de l'autre (PROTOCOL.md §5.3.1). `dp100` pose donc un verrou exclusif (`flock`) sur le nœud hidraw : un second processus est refusé avec un message explicite.

Le déploiement recommandé est un service systemd `dp100-mcp` en Streamable HTTP sur `127.0.0.1:8765`, propriétaire de l'appareil, auquel tous les agents se connectent.

```bash
sudo apt install build-essential pkg-config libudev-dev
```

```bash
cargo build --release && sudo packaging/install.sh
```

[`packaging/install.sh`](packaging/install.sh) est idempotent. Il installe :
- les binaires dans `/usr/local/bin` ;
- la règle udev ;
- un utilisateur système `dp100` (groupe `plugdev`) ;
- la config `/etc/dp100-mcp/config.toml`, sauf si elle existe déjà ;
- la doc dans `/usr/local/share/doc/dp100-mcp/` ;
- l'unité systemd, qu'il active et démarre.

Ensuite, pour chaque client :

```bash
claude mcp add --scope user --transport http dp100 http://127.0.0.1:8765/mcp
```

Pour OpenCode, utilise l'entrée `remote` de [`examples/opencode.json`](examples/opencode.json), avec `"enabled": true`.

Exploitation courante :
- **modifier les limites** : édite `/etc/dp100-mcp/config.toml`, puis `sudo systemctl restart dp100-mcp` ;
- **logs** : `journalctl -u dp100-mcp -f` ;
- **utiliser `dp100ctl`** pendant que le service tourne : arrête d'abord le service avec `sudo systemctl stop dp100-mcp` (avec `on_exit = "off"`, la sortie est coupée), sinon `dp100ctl` est refusé par le verrou.

### Installation manuelle

```bash
cargo build --release
```

```bash
sudo install -m 0755 target/release/dp100-mcp target/release/dp100ctl /usr/local/bin/
```

### Accès USB sans root (udev)

La DP100 doit être en mode **USBD** (double appui sur ◀) et reliée par son port USB-A. Par défaut, le nœud `/dev/hidraw*` est `root:root 0600`. La règle fournie le donne au groupe `plugdev` :

```bash
sudo install -m 0644 packaging/99-dp100.rules /etc/udev/rules.d/
```

```bash
sudo udevadm control --reload-rules && sudo udevadm trigger
```

```bash
sudo usermod -aG plugdev "$USER"
```

Ensuite, déconnecte-toi puis reconnecte-toi, et vérifie avec `dp100ctl list`.

## CLI

```bash
dp100ctl status
```

```bash
dp100ctl --max-voltage 12 set --voltage 3.3 --current 0.2
```

```bash
dp100ctl on
```

```bash
dp100ctl cycle --off-ms 2000
```

```bash
dp100ctl watch --rate-hz 20 --duration-s 10 --csv boot.csv
```

Options globales : `--serial`, `--config`, `--max-voltage`, `--max-current`, `--vin-dropout-mv`, `--json`, `--trace-proto` (dump hex TX/RX sur stderr). `DP100_LOG=debug` règle la verbosité.

## Serveur MCP

### Outils

| Outil | Effet |
|---|---|
| `list_devices` | DP100 connectées (série USB, firmware, HW) et limites du serveur |
| `get_status` | Vin, Vout, Iout, P, températures, ON/OFF, CV/CC, protection déclenchée, consignes |
| `set_output` | `voltage_v`, `current_limit_a`, `enable` (optionnel) ; renvoie la relecture et la télémétrie |
| `output_on` / `output_off` | Allume / éteint la sortie |
| `power_cycle` | `off_ms`, rallumage avec les consignes courantes, mesures avant et après |
| `measure` | `duration_ms`, `rate_hz` (≤ 200) : min/max/moyenne V/I/P, énergie (mWh), charge (mAh), `csv_path` optionnel |
| `ramp_voltage` | `start_v` → `end_v` par `step_v` toutes les `dwell_ms`, mesure à chaque palier, arrêt si une protection se déclenche |
| `get_presets` / `set_preset` / `apply_preset` | Presets P0–P9. Écriture et rappel sur P1–P9 seulement : P0 est refusé, comme par la DLL du fabricant |
| `get_settings` / `set_settings` | OTP, OPP, rétroéclairage, volume, protection d'inversion, sortie à la mise sous tension, OVP/OCP du jeu actif |

Tous les outils acceptent un `serial` optionnel ; il est requis si plusieurs DP100 sont branchées. Ils renvoient du JSON structuré (`structuredContent`). Les erreurs sont des résultats `isError` lisibles par l'agent.

### Claude Code (stdio)

Seulement si aucun service ne tourne : un seul processus peut tenir l'appareil.

```bash
claude mcp add dp100 -- dp100-mcp --max-voltage 12 --max-current 1 --on-exit off
```

Ou bien un fichier `.mcp.json` à la racine du projet ; voir [`examples/.mcp.json`](examples/.mcp.json).

### Claude Code (HTTP)

Lance le serveur (ou utilise l'unité systemd ci-dessous) :

```bash
dp100-mcp --http 127.0.0.1:8765 --config /etc/dp100-mcp/config.toml
```

Puis :

```bash
claude mcp add --transport http dp100 http://127.0.0.1:8765/mcp
```

### OpenCode

Voir [`examples/opencode.json`](examples/opencode.json). Le serveur `local` lance `dp100-mcp` en stdio, le serveur `remote` pointe sur `http://127.0.0.1:8765/mcp`.

### Mode HTTP : points d'attention

- Seuls les en-têtes `Host` loopback sont acceptés, contre le DNS rebinding. Pour en ajouter : `--allowed-host`, ou `server.allowed_hosts` dans le TOML.
- Il n'y a **aucune authentification** : n'écoute pas sur une adresse non loopback sans reverse proxy authentifié.
- En HTTP, `measure` n'écrit de CSV que si `--csv-dir` est fourni, et seulement dans ce répertoire. En stdio, tout chemin absolu est accepté.

### Unité systemd

[`packaging/dp100-mcp.service`](packaging/dp100-mcp.service) lance le mode HTTP sur `127.0.0.1:8765` sous l'utilisateur système `dp100` (groupe `plugdev`), avec un sandboxing qui n'autorise que les périphériques hidraw. Les CSV de `measure` vont dans `/var/lib/dp100-mcp`. Elle est installée par `packaging/install.sh`.

## Configuration TOML

Voir [`examples/dp100-mcp.toml`](examples/dp100-mcp.toml). Les options de ligne de commande priment sur le fichier.

```toml
[device]
serial = "3F0F22964000"   # optionnel

[limits]
max_voltage_v = 12.0
max_current_a = 1.0
vin_dropout_mv = 500

[server]
on_exit = "off"           # keep | off
```

## Tests

Sans matériel : codec, limites, actor et opérations contre un simulateur fidèle au protocole.

```bash
cargo test --workspace
```

```bash
cargo clippy --workspace --all-targets -- -D warnings
```

Matériel, en lecture seule :

```bash
cargo test -p dp100 --features hil --test hil
```

Matériel avec écriture (consigne modifiée, sortie maintenue OFF puis restaurée). **Rien ne doit être branché sur la sortie** :

```bash
DP100_HIL_ALLOW_OUTPUT=1 cargo test -p dp100 --features hil --test hil -- --ignored --test-threads=1
```

`dp100-mcp --simulate` et `dp100ctl --simulate` font tourner le serveur ou la CLI contre une DP100 simulée.

## Points non encore vérifiés sur le matériel

Voir PROTOCOL.md §9 : les presets (`0x40`, `0x80`), l'écriture des réglages système, et les codes `work_st` autres que 0. Sont vérifiés sur le matériel : consignes, allumage et coupure (à vide et en charge), CC/CV/OFF, unité de Iout, et polling à 100 Hz. Le serveur relit systématiquement l'état, donc un écart éventuel remonte sous forme de `warnings`.
