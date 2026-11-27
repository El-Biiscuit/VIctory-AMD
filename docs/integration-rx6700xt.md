# Branche `rx6700xt-integration`

Branche d'intégration de PR ouvertes sur [DexNom/VIctory](https://github.com/DexNom/VIctory), choisies pour une **AMD Radeon RX 6700 XT** (RDNA2, gfx1031, 12 Go).

Base : `main` = `DexNom/VIctory@4ad8692`. Au moment de la sélection, 148 PR étaient ouvertes upstream et aucune n'était mergée.

## Ce qui distingue la 6700 XT de la carte de référence (RX 7900 XTX, 24 Go)

- **Pas de FSR 4.** `crates/victory-hle/src/modules/fidelityfx.rs` refuse le provider s'il n'annonce pas de version `4.x`. Sur RDNA2, utilise `--native-1440p` ou `--1080p-to-1440p` au lieu de `--fsr-quality`.
- **Wave64 natif**, comme RDNA3. Les PR qui visent les hôtes 32-lane (NVIDIA) sont donc sans objet.
- **12 Go de VRAM** et un GPU plus lent. Les priorités sont la mémoire, la stabilité du temps de frame et le watchdog de 30 s du jeu pendant les compilations à froid.

## Intégré

| PR | Sujet | Pourquoi pour la 6700 XT |
| --- | --- | --- |
| #54 (partie shader) | `ds_bpermute` wave64 traité par demi-wave de 32 lanes (ISA RDNA2 §10.4.4), lookups BVH | Corrige la sémantique des shaders du jeu sur un hôte wave64 (masques d'ombre en damier). Voir le commit pour la résolution de conflit. |
| #17 | Thin LTO, un seul codegen unit | Gain CPU gratuit (hachage, copies, traduction). Le build est plus long. |
| #121 | Pipelines du builder BVH mis en file dès leur création et compilés en parallèle | Évite que les 53 pipelines compilés en série à froid déclenchent le watchdog de 30 s, ce qui est plus probable sur une carte plus lente. `VICTORY_BVH_BUILDER_EARLY=0` pour désactiver. |
| #18 | Table des vues en `RwLock` | Moins de contention entre enregistreurs de command lists. |
| #129 | Classes de taille d'upload de textures sur des verrous séparés | Moins de contention au streaming. |
| #85 | Revérification des textures conservée quand un slot de descripteur est réutilisé | Moins de textures périmées ou noires. |
| #131 | Le refresh sparse saute les mips fines indisponibles | Utile avec `--sparse-textures`. |
| #72 | Ne redétile que les mips et slices réellement écrites | Moins de CPU et d'uploads. |
| #110 | Bibliothèques de pipelines publiées dans l'ordre de génération | Cache de pipelines plus fiable entre deux lancements. |
| #111 | Une traduction réussie est conservée même si le cache disque échoue | Robustesse. |
| #139, #140, #146, #145, #81, #30 | Correctifs ISA (SCC de s_min/s_max, lane de v_writelane, littéraux 64 bits signés, v_cvt_off_f32_i4, f16 interp op_sel, longueur VOP1 et destination VOP3 CMPX) | Exactitude des shaders traduits, indépendante du GPU. |
| #142, #143, #144 | Samplers (anisotrope et mip point, CreateSamplerX) et attributs de rectangles normalisés | Exactitude du rendu. |

## Écarté, et pourquoi

- **#75 (retirée après test sur la 6700 XT, 28/11).** Elle coupe le rattrapage accéléré des textures dès que la marge de commit du *système* (`ullAvailPageFile`) passe sous 4 Gio. En jeu, le runtime monte à 20–23 Go de RAM : la condition est vraie en permanence, les textures en attente montent à environ 8 000 et ne se rattrapent pas (visages et matériaux manquants, pire que `main`).
- **#60 (limite de taille des mips des textures streamées)**, pourtant testée sur une 6700 XT (10,0 → 8,6 Go). Elle a été écrite avant la refonte du staging et du sparse des textures de `main` (`ab34a8b`). Le portage n'est plus une résolution de conflit : le staging, `note_texture_mips`/`texture_covers`, les uploads « ahead » et les préludes sparse supposent tous que les mips du jeu et de l'hôte ont la même numérotation. Un portage à l'aveugle risque des textures corrompues ou une perte du device. À refaire dans une branche séparée, testée en jeu.
- **#43** sérialise toute la création de PSO sur AMD (contournement RDNA4). Ça ralentirait la compilation à froid sur RDNA2 sans rien apporter.
- **#45, #56, #7, #23, #8** : contournements de crashs du compilateur RDNA4 (`amdxc64.dll`). #23 (garde SEH) n'est utile qu'en cas de crash dans `amdxc64.dll`.
- **#14, #15, #22, #35, #40, #64, #98–101** : hôtes 32-lane (NVIDIA) et grosses piles de commits.
- **#114, #115, #130** : merges de forks de +500 commits, impossibles à relire.
- **#134** (frame generation) : se greffe sur le preset FSR 4, indisponible sur RDNA2.
- **#25** : en conflit avec `main`, gain marginal.
- Docs seules, Linux/Proton (#6, #13, #28, #36–39, #42), Windows 10 (#12) : hors sujet ici.

## Vérifications faites

- `victory-shader` : 187 tests passent sous Linux (182 sur `main`). Les tests wave64 de #54 n'existent que sous Windows avec un GPU et n'ont pas tourné.
- `victory-gpu` (lib et tests) : typage complet avec `cfg(windows)` forcé sous Linux, 0 erreur sur la branche comme sur `main`, une fois neutralisées les seules API `std::os::windows` absentes sous Linux.
- **Non fait** : build MSVC réel, exécution du jeu, mesures de frame time et de VRAM.

## À tester sur ta machine

```powershell
.\Setup.cmd
& "$env:LOCALAPPDATA\DexNom Runtime\victory-play.exe" --rt --console series-x --local-gpu-memory --native-1440p --sparse-textures --no-upload-ahead
```

Compare avec `main` sur la même sauvegarde et la même caméra : frame time médian et p95, VRAM dédiée et partagée, draws perdus, présence ou non du watchdog au premier chargement avec ray tracing (`logs\latest.txt`).
