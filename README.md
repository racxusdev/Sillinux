# Sillinux

**Português** · [English](README.en.md)

Ferramenta **nativa para Linux** (Rust, sem Wine) para cortadores **Silhouette / Graphtec**: importa imagens e SVG, edita por camadas, **vetoriza** com quatro motores (incluindo curvas de qualidade Studio), configura e envia o corte pela **USB** direto usando o protocolo **GPGL** (1 unidade = 0,05 mm) ou por **Bluetooth** sem root. Inclui **marcas de registro** para fluxo *print & cut* com impressão via CUPS.

<img width="1318" height="905" alt="imagem" src="https://github.com/user-attachments/assets/15860ab6-bd8b-4b65-9dcd-2ca02b982d8f" />

Dois programas:

| binário | o que faz |
|---|---|
| `silhouette` | linha de comando: inspecionar `.studio3`/SVG, folha de impressão, corte com marcas de registro, teste dourado |
| `silhouette-gui` | janela gráfica: canvas com zoom/pan, camadas de imagem e vetor, vetorização, texto, print & cut, corte com progresso |

## AppImage (portátil, sem instalar)

```bash
chmod +x Sillinux-x86_64.AppImage
./Sillinux-x86_64.AppImage
```

Sem FUSE no sistema? `./Sillinux-x86_64.AppImage --appimage-extract-and-run`.

O AppImage mira **glibc 2.17** (Ubuntu 14.04+/Debian 8+/CentOS 7+ em diante) e a **libusb é compilada embutida e estática** — o binário depende só da glibc; nada de libudev, libusb ou X11 no sistema alvo. Gera-se com:

```bash
./packaging/build-appimage.sh   # requer cargo-zigbuild + zig no PATH
# resultado: dist/Sillinux-x86_64.AppImage
```

## Permissões USB (obrigatório, uma vez)

Sem isso o dispositivo lista mas não abre. Pela GUI: botão **"Instalar regra udev (USB, só uma vez)"** em **Ligações** (pede a senha via polkit). No terminal:

```bash
sudo cp packaging/udev/99-silhouette.rules /etc/udev/rules.d/
sudo udevadm control --reload-rules && sudo udevadm trigger
# desconecte e reconecte a máquina
```

A regra dá acesso ao usuário comum (`uaccess`, com fallback `plugdev`) a qualquer Silhouette/Graphtec (`VID 0b4d`).

**Bluetooth não precisa de nada disso**: a GUI abre a porta série SPP com um socket RFCOMM direto como usuário comum — sem root, sem `rfcomm`, sem polkit.

## A GUI

### Vetorização (o coração do app)

Quatro métodos, todos com máscara por **distância de cor do fundo** — arte colorida sobre fundo claro (ou escuro) é traçada inteira, inclusive peças claras que escapam de qualquer limiar de luminância:

| Método | o que faz |
|---|---|
| **Auto (recomendado)** | vtracer + limiar de Otsu + tolerância automática; ajusta tudo sozinho |
| **Apenas contorno** | marching squares por limiar, furos incluídos, polimento uniforme |
| **Todas as linhas (bordas)** | esqueleto por Sobel para desenho a traço |
| **Qualidade Studio (curvas)** | vtracer (agrupamento de cores + ajuste de curvas Bézier) |

Recursos em volta da vetorização:

- **⬚ Vetorizar área** — arraste uma área sobre a imagem e vetorize só ela com **o dobro da resolução**: é como resgatar detalhes pequenos que escaparam.
- **Cor por imagem** — tonalidade, saturação, vibração, contraste e brilho por imagem (não globais); o corte segue a cor ajustada.
- **Polimento automático** — reamostragem uniforme + suavização que preserva cantos + autocorreção proporcional à rugosidade: curvas lisas, cantos nítidos.
- **Manchas (%)** — remoção de ruído estilo Studio; **Apenas contorno externo** e **offset externo já aplicado** como opções.
- **Pré-visualização ao vivo** em thread separada — a interface nunca trava.
- **⌒ Arredondar** — pós-vetorização: simplifica arredondado, sem ficar quadrado, preservando cantos reais.
- **Edição de nós** — adicionar (duplo clique), remover (clique direito) e arrastar pontos; **Separar objetos** para desagrupar peça a peça; **recortar dentro/fora da área** arrastada.

### Camadas e imagens

- **Múltiplas imagens**, cada uma como camada: mostrar/esconder, reordenar (▲▼), apagar; **vetor nasce independente** da imagem (e pode ser **separado da imagem** depois, no clique direito).
- **Texto** com **fontes personalizadas** (importar TTF/OTF — ficam salvas), preview de cada fonte desenhada nela mesma, cor e tamanho ao vivo.
- **Undo/redo 60 níveis** (Ctrl+Z / Ctrl+Y) — inclusive restaura **imagens apagadas**; `Ctrl+D` duplica; `Delete` apaga; `Ctrl+G` agrupa.
- Idioma **PT / EN** na barra superior; preferências salvas em `~/.config/sillinux/prefs.json`.
- **Deteção automática de USB**: liga/desliga a máquina e o app avisa.

### Corte e máquinas

- **Duas máquinas simultâneas** (USB ou Bluetooth) com firmware/estado ao vivo, corte em thread separada com progresso.
- **Print & cut**: marcas no padrão do Studio (quadrado 10×10 mm + "L"), busca automática `TB123` (ou `TB23` manual), compensação de distorção; folha de impressão exportada e enviada via **CUPS** (Epson e genéricas).
- Ajustes de sessão salvos automaticamente (método de traçado, impressão, corte, impressora, idioma).
- Ações de conexão usam socket/libusb direto — **sem `bluetoothctl`, sem `rfcomm`, sem sudo** no fluxo Bluetooth.

### CLI

```bash
silhouette inspect arquivo.studio3        # página, caminhos, imagem embutida
silhouette svg arquivo.studio3 -o saida.svg
silhouette cut desenho.svg --page 210x297 --force 4 --speed 3
silhouette cut folha.svg --regmarks --reg-width 180 --reg-length 230
silhouette regmarks --page A4 --square-mm 10 --arm-mm 10 -o marcas.svg
silhouette print-sheet desenho.png -o folha.pdf
silhouette golden                          # teste de quadrado 30 mm validado
silhouette probe                            # firmware/status da máquina
```

`cut --dry-run` valida o pipeline e grava o programa GPGL em texto (comandos `FG`, `TB50,0`, `M/D`, `TB123`…) sem tocar na máquina.

## Compilar

```bash
cargo build --release        # binários em target/release/
cargo test                   # 31 core + 10 driver + GUI
```

A libusb é compilada embutida (`rusb` com `vendored`) — não precisa de `libusb-1.0-dev`. Para o AppImage portátil (glibc 2.17): `cargo-zigbuild` + `zig 0.14` no `PATH` e `./packaging/build-appimage.sh`.

## Máquinas suportadas

**Cameo 5** (`0b4d:1140`) é o hardware onde o protocolo foi **validado em corte real** (quadrado dourado, regmarks, status). A família `0b4d` (Graphtec/Silhouette: Cameo 1–5, Portrait, SD, Craft Robo) usa o mesmo GPGL e deve funcionar — relatórios bem-vindos.

## Estrutura

```
crates/silhouette-core    GPGL encoder, geometria, traçado (marching squares + vtracer), marcas
crates/silhouette-driver  transportes: USB (libusb estático), Bluetooth RFCOMM, serial + sessão
crates/silhouette-app      CLI
crates/silhouette-ui      GUI (egui), importação (SVG/PDF/PSD/raster/Studio3), i18n, preferências
packaging/                build do AppImage, regra udev, desktop, metainfo
docs/                     notas de formato e engenharia reversa do GPGL
```

## Limitações honestas

- **Print & cut**: imprima sempre em **100 %**; a busca de marcas `TB123` está implementada, mas ainda **não validada fisicamente** na Cameo 5 (o quadrado dourado e o status já foram validados em corte real).
- **`.studio3`**: leitura dos caminhos e da maior imagem embutida; não abre tudo que o Studio guarda.
- **Sem DXF** e sem `<text>` de SVG — texto use a ferramenta Texto da GUI.
- **Multi-suporte Cameo 4/5**: envia para a ferramenta 1 (`J1`).
- **Vincar/esboçar** ainda não expostos na GUI (o protocolo tem `FN`/ferramenta 2).

## Licença

MIT OR Apache-2.0. Os fatos de protocolo vêm de documentação de projeto GPL (inkscape-silhouette) e de sessões próprias de engenharia reversa (`docs/`); a implementação é original.

---

☕ **Gostou do Sillinux?** Se ele te poupou tempo (e/ou uma dor de cabeça), um café é sempre bem-vindo — e ajuda a manter o projeto vivo: [**ko-fi.com/racxus**](https://ko-fi.com/racxus) 💛
