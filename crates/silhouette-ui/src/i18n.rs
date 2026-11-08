//! Two-language UI: Portuguese (PT-PT, the source strings) and English.
//!
//! Panel/button strings are wrapped with `self.t("…")`; `tr` returns the key
//! unchanged for PT and the English equivalent for EN. Keys are the PT
//! literals, so a missing translation falls back to Portuguese instead of
//! breaking the layout.

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Pt,
    En,
}

impl Lang {
    pub fn other(self) -> Self {
        match self {
            Lang::Pt => Lang::En,
            Lang::En => Lang::Pt,
        }
    }

    /// Label shown on the toggle button (the language you would switch to).
    pub fn toggle_label(self) -> &'static str {
        match self {
            Lang::Pt => "EN",
            Lang::En => "PT",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Lang::Pt => "pt",
            Lang::En => "en",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "en" => Some(Lang::En),
            "pt" => Some(Lang::Pt),
            _ => None,
        }
    }
}

pub fn tr(lang: Lang, pt: &'static str) -> &'static str {
    if lang == Lang::Pt {
        return pt;
    }
    match pt {
        "A app lê as marcas antes de cortar; uma leitura falhada cancela o corte." => {
            "The app reads the marks before cutting; a failed read cancels the cut."
        }
        "A leitura de marcas no Cameo 5 ainda não está validada fisicamente." => {
            "Registration-mark reading on the Cameo 5 is not yet physically validated."
        }
        "A lâmina vai mover-se sobre o tapete." => "The blade will move over the mat.",
        "A4 a 100 %, sem ajustar à página." => "A4 at 100 %, no fit-to-page.",
        "Abra uma imagem (.png, .jpg, .webp, .tiff, .psd)." => {
            "Open an image (.png, .jpg, .webp, .tiff, .psd)."
        }
        "Abra uma imagem e use «Vetorizar imagem»." => {
            "Open an image and use “Trace image”."
        }
        "Abrir ficheiro (Ctrl+O)" => "Open file (Ctrl+O)",
        "Abrir imagens" => "Open images",
        "Bem-vindo ao Sillinux" => "Welcome to Sillinux",
        "Cortar na Cameo em três passos:" => "Cut with the Cameo in three steps:",
        "1 · Abra uma ou mais imagens (.png, .jpg, .svg, .pdf, .psd…)." => {
            "1 · Open one or more images (.png, .jpg, .svg, .pdf, .psd…)."
        }
        "2 · Ajuste a cor se quiser e use «Vetorizar imagem» para criar as linhas." => {
            "2 · Adjust the colour if you like and use “Trace image” to create the lines."
        }
        "3 · Em «Ligações» ligue a máquina e confirme o corte." => {
            "3 · In “Connections” connect the machine and confirm the cut."
        }
        "Instalar regra udev (USB)" => "Install udev rule (USB)",
        "Ajuda" => "Help",
        "Não mostrar novamente" => "Don’t show again",
        "Mudar o idioma / Switch language" => "Switch language / Mudar o idioma",
        "Adicionar texto" => "Add text",
        "Afastar" => "Zoom out",
        "Ajuda e comandos" => "Help & shortcuts",
        "Ajustar à janela" => "Fit to window",
        "Ajustar à página" => "Fit to page",
        "Ajuste a distância do offset no painel «Vetorizar»." => {
            "Adjust the offset distance in the “Trace” panel."
        }
        "Alinhar" => "Align",
        "Apagar" => "Delete",
        "Aplicar" => "Apply",
        "Aproximar" => "Zoom in",
        "Arraste pontos/traços e pegas." => "Drag points/paths and handles.",
        "Arte" => "Artwork",
        "As marcas impressas têm de corresponder ao painel Marcas de registo." => {
            "The printed marks must match the Registration marks panel."
        }
        "Atalhos" => "Shortcuts",
        "Atualizar estado" => "Refresh status",
        "Braço dos L" => "L-mark arm",
        "Brilho" => "Brightness",
        "Busca começa N mm antes das marcas; padrão 10 mm." => {
            "Search starts N mm before the marks; default 10 mm."
        }
        "Busca das marcas (TB123)" => "Mark search (TB123)",
        "Camadas" => "Layers",
        "Cancelar" => "Cancel",
        "Caneta" => "Pen",
        "Cantos escalam proporcionalmente; Shift liberta o aspeto; Alt/Ctrl escala pelo centro." => {
            "Corners scale proportionally; Shift unlocks the ratio; Alt/Ctrl scales from the centre."
        }
        "Centrar no canvas" => "Centre on canvas",
        "Centro/pontas da página, centros e arestas dos objetos." => {
            "Page centre/edges, object centres and edges."
        }
        "Confirmar corte" => "Confirm cut",
        "Confirme a origem da página e a lâmina instalada." => {
            "Check the page origin and the blade installed."
        }
        "Contraste" => "Contrast",
        "Converter em caminho" => "Convert to path",
        "Cor" => "Colour",
        "Cortar em" => "Cut with",
        "Corte" => "Cut",
        "Desfazer (Ctrl+Z)" => "Undo (Ctrl+Z)",
        "Design" => "Design",
        "Desligar" => "Disconnect",
        "Direção: 0=vertical, 1=horizontal" => "Direction: 0=vertical, 1=horizontal",
        "Dispositivo" => "Device",
        "Distância" => "Distance",
        "Dry-run (não envia)" => "Dry-run (does not send)",
        "Efeitos só na imagem selecionada; a cor de cada camada define-se em «Camadas»." => {
            "Effects apply to the selected image only; each layer’s colour is set in “Layers”."
        }
        "Encaixe" => "Snapping",
        "Entre si" => "To each other",
        "Escala" => "Scale",
        "Escreve a regra em /etc/udev/rules.d e recarrega o udev; pede a senha uma vez" => {
            "Writes the rule to /etc/udev/rules.d and reloads udev; asks for the password once"
        }
        "Espessura" => "Thickness",
        "Espessura 0,3 mm; imprimir a 100 % e usar a mesma geometria na leitura (TB123)." => {
            "Thickness 0.3 mm; print at 100 % and reuse the same geometry when reading (TB123)."
        }
        "Estilo" => "Style",
        "Exportar SVG com linhas de corte" => "Export SVG with cut lines",
        "Externo +" => "Outer +",
        "Ferramenta" => "Tool",
        "Ficheiro" => "File",
        "Fila" => "Queue",
        "Fonte" => "Font",
        "Força" => "Force",
        "Gerar folha A4 com marcas" => "Generate A4 sheet with marks",
        "Guardar PDF..." => "Save PDF...",
        "Guardar projeto SVG (Ctrl+S)" => "Save project SVG (Ctrl+S)",
        "Guias inteligentes" => "Smart guides",
        "Imagem" => "Image",
        "Importar fonte" => "Import font",
        "Impressora" => "Printer",
        "Impressão" => "Printing",
        "Imprima a 100 % e não altere a folha depois." => {
            "Print at 100 % and do not alter the sheet afterwards."
        }
        "Interno −" => "Inner −",
        "Ligar Bluetooth" => "Connect Bluetooth",
        "Ligar USB" => "Connect USB",
        "Ligar impressora" => "Connect printer",
        "Ligações" => "Connections",
        "Ligações mostra sempre: Silhouette 1, Silhouette 2 e Impressora." => {
            "Connections always shows: Silhouette 1, Silhouette 2 and Printer."
        }
        "Limiar de tinta" => "Ink threshold",
        "Limites do Studio: braço 6–30 mm, espessura 0,3–1,0 mm, inset ≥ 10 mm." => {
            "Studio limits: arm 6–30 mm, thickness 0.3–1.0 mm, inset ≥ 10 mm."
        }
        "Linha de comandos" => "Command line",
        "Linhas de corte" => "Cut lines",
        "Lâmina" => "Blade",
        "M1" => "M1",
        "M2" => "M2",
        "M2 desmarcada não recebe o trabalho." => "Unticked M2 does not receive the job.",
        "Marcas" => "Marks",
        "Marcas de registo" => "Registration marks",
        "Margem (inset)" => "Margin (inset)",
        "Material" => "Media",
        "Modo dry-run: nada será enviado." => "Dry-run mode: nothing will be sent.",
        "Mova os controlos para ver a vetorização antes de aplicar." => {
            "Move the controls to preview the trace before applying."
        }
        "Método" => "Method",
        "Mín." => "Min.",
        "Nunca imprimir para a fila CAMEO-5: é a cortadora." => {
            "Never print to the CAMEO-5 queue: that is the cutter."
        }
        "Offset esquerda (mm)" => "Offset left (mm)",
        "Offset topo (mm)" => "Offset top (mm)",
        "Papel" => "Paper",
        "Passo (mm)" => "Step (mm)",
        "Passo > 0 habilita marcas segmentadas (TB23 repetido)." => {
            "Step > 0 enables segmented marks (repeated TB23)."
        }
        "Passo da grelha" => "Grid step",
        "Permite ao utilizador abrir a máquina sem sudo (emparelhamento Bluetooth não precisa disto)." => {
            "Lets the desktop user open the machine without sudo (Bluetooth pairing does not need this)."
        }
        "Profundidade" => "Depth",
        "Progresso" => "Progress",
        "Página" => "Page",
        "Quadrado" => "Square",
        "Quadrado de 5 mm no canto superior esquerdo e três L com braço de 20 mm." => {
            "5 mm square at the top-left corner and three L marks with a 20 mm arm."
        }
        "Qualidade" => "Quality",
        "Qualidade, cor e papel usam os valores do driver Epson (lpoptions -l)." => {
            "Quality, colour and paper use the Epson driver values (lpoptions -l)."
        }
        "Registo de cortes e leituras: ~/.cache/sillinux/cut.log" => {
            "Cut and read log: ~/.cache/sillinux/cut.log"
        }
        "Registo: ~/.cache/sillinux/cut.log" => "Log: ~/.cache/sillinux/cut.log",
        "Repor cor" => "Reset colour",
        "Repor posição" => "Reset view",
        "Ruído (mm²)" => "Noise (mm²)",
        "Saturação" => "Saturation",
        "Se a leitura falhar, confirme a escala de impressão e reimprima as marcas." => {
            "If the read fails, check the print scale and reprint the marks."
        }
        "Segmentos (mídia longa)" => "Segments (long media)",
        "Sem leitura de marcas: o corte usa a origem da página." => {
            "No mark reading: the cut uses the page origin."
        }
        "Sensibilidade" => "Sensitivity",
        "Separar linhas" => "Separate lines",
        "Simplif." => "Simplify",
        "Suavizar" => "Smoothing",
        "Qualidade Studio (curvas)" => "Studio quality (curves)",
        "🔓 Separar da imagem" => "🔓 Detach from image",
        "A linha deixa de acompanhar a imagem e vira independente" => {
            "The line stops following the image and becomes independent"
        }
        "✂ Separar objetos" => "✂ Split into objects",
        "⌒ Arredondar seleção" => "⌒ Round up selection",
        "⌒ Arredondar camada" => "⌒ Round up layer",
        "Simplifica as linhas selecionadas de forma arredondada (tolerância do «Simplif.»)" => {
            "Simplifies the selected lines the rounded way («Simplify» tolerance)"
        }
        "Simplifica esta linha de forma arredondada (tolerância do «Simplif.»)" => {
            "Simplifies this line the rounded way («Simplify» tolerance)"
        }
        "Desagrupa: cada parte do vetor fica selecionável sozinha" => {
            "Ungroups: every vector part becomes selectable on its own"
        }
        "As linhas deixam de acompanhar a imagem" => "The lines stop following the image",
        "Auto (recomendado)" => "Auto (recommended)",
        "⬚ Vetorizar área" => "⬚ Trace area",
        "Parâmetros ajustados automaticamente." => "Parameters tuned automatically.",
        "Arraste uma área no canvas e vetorize só ela, com o dobro da resolução — bom para detalhes pequenos" => {
            "Drag an area on the canvas and trace only it, at double resolution — great for small details"
        }
        "Manchas (%)" => "Speckles (%)",
        "Manchas altas removem peças pequenas soltas" => {
            "High speckle removes small loose pieces"
        }
        "Offset externo ATIVO: os contornos nascem deslocados para fora" => {
            "External offset ACTIVE: contours are born shifted outwards"
        }
        "Apenas contorno externo" => "Outer contour only",
        "Aplicar offset externo" => "Bake external offset",
        "✂ Manter só dentro da área" => "✂ Keep only inside the area",
        "🗑 Apagar dentro da área" => "🗑 Delete inside the area",
        "Recorta as linhas selecionadas à área arrastada" => {
            "Clips the selected lines to the dragged area"
        }
        "Remove as partes dentro da área arrastada" => {
            "Removes the parts inside the dragged area"
        }
        "Parâmetros ajustados automaticamente." => "Parameters tuned automatically.",
        "Snap à grelha" => "Snap to grid",
        "Tamanho" => "Size",
        "Tamanho 100 %" => "Size 100 %",
        "Texto" => "Text",
        "Tonalidade" => "Hue",
        "Traços" => "Strokes",
        "Três identificadores" => "Three targets",
        "Use A4, 100 %, sem ajustar à página. A app chama:" => {
            "Use A4, 100 %, no fit-to-page. The app calls:"
        }
        "Velocidade" => "Speed",
        "Ver estado do trabalho" => "View job status",
        "Vetorizar imagem" => "Trace image",
        "Vetorização e offset" => "Trace & offset",
        "Vibração" => "Vibrance",
        "X" => "X",
        "Y" => "Y",
        "apagar imagem e os traços dela" => "delete the image and its traced paths",
        "apagar traço" => "delete path",
        "atualizar" => "refresh",
        "centrar na horizontal da página" => "centre horizontally on the page",
        "centrar na vertical da página" => "centre vertically on the page",
        "cor da camada" => "layer colour",
        "cor do texto (preenchimento)" => "text colour (fill)",
        "editar nós desta camada" => "edit this layer’s nodes",
        "encostar ao topo da página" => "snap to the top of the page",
        "encostar à base da página" => "snap to the bottom of the page",
        "encostar à direita da página" => "snap to the right of the page",
        "encostar à esquerda da página" => "snap to the left of the page",
        "enviar para trás" => "send backward",
        "imagens" => "images",
        "imprime uma página A4 de teste" => "prints a test A4 page",
        "mostrar linhas de corte" => "show cut lines",
        "mostrar marcas de registo" => "show registration marks",
        "mostrar/esconder imagem" => "show/hide image",
        "mover a imagem e a vetorização" => "move the image and its traced paths",
        "repor original" => "restore original",
        "selecione uma imagem (clique nela na folha)" => {
            "select an image (click it on the sheet)"
        }
        "trazer para frente" => "bring forward",
        "«Testar marcas» lê as marcas sem cortar e mostra a resposta do sensor." => {
            "“Test marks” reads the marks without cutting and shows the sensor reply."
        }
        "• A impressora mostra a fila CUPS e o estado (pronta / a imprimir)." => {
            "• The printer shows the CUPS queue and state (ready / printing)."
        }
        "• Apagar vetorização remove todas as linhas geradas; Delete apaga a seleção." => {
            "• Delete trace removes all generated lines; Delete clears the selection."
        }
        "• Apenas contorno: segue a silhueta do conteúdo." => {
            "• Outline only: follows the silhouette of the content."
        }
        "• Atualizar estado relê firmware/estado; Desligar liberta a porta." => {
            "• Refresh status re-reads firmware/state; Disconnect releases the port."
        }
        "• Cada Silhouette liga por USB ou Bluetooth e mostra firmware e estado." => {
            "• Each Silhouette connects by USB or Bluetooth and shows firmware and state."
        }
        "• Em Corte marque M1 e/ou M2; M2 desmarcada não recebe o trabalho." => {
            "• In Cut tick M1 and/or M2; unticked M2 does not receive the job."
        }
        "• Ferramenta Imagem: arraste a imagem; a vetorização nasce separada." => {
            "• Image tool: drag the image; the traced paths are independent."
        }
        "• Offset externo/interno mostra uma pré-visualização; Aplicar confirma, Cancelar descarta." => {
            "• Outer/inner offset shows a preview; Apply confirms, Cancel discards."
        }
        "• Separar linhas: cada contorno fica editável; desmarcado, ficam agrupados." => {
            "• Separate lines: each contour becomes editable; unticked, they stay grouped."
        }
        "• Simplificar e comprimento mínimo refinam; Vetorizar substitui o anterior." => {
            "• Simplify and minimum length refine; Trace replaces the previous one."
        }
        "• Todas as linhas: deteta bordas e reduz a linhas de 1 px." => {
            "• All lines: detects edges and reduces to 1 px lines."
        }
        "• «Ler marcas» procura o quadrado e os L antes de cortar; se falhar, não corta." => {
            "• “Read marks” looks for the square and the Ls before cutting; if it fails, it will not cut."
        }
        "↻ Girar 90°" => "↻ Rotate 90°",
        "⌖ Marcas" => "⌖ Marks",
        "▦ Grelha" => "▦ Grid",
        "✂ Corte" => "✂ Cut",
        "✂️ Silhueta (contorno externo)" => "✂️ Silhouette (outer contour)",
        "✎ Editar nós" => "✎ Edit nodes",
        "✕ Cancelar" => "✕ Cancel",
        "✕ Limpar pré-visualização" => "✕ Clear preview",
        "❓ Ajuda e comandos" => "❓ Help & shortcuts",
        "⤴ Offset externo" => "⤴ Outer offset",
        "⤴ Offset externo da imagem" => "⤴ Outer offset of image",
        "⤴ Offset externo da seleção" => "⤴ Outer offset of selection",
        "⤵ Offset interno" => "⤵ Inner offset",
        "⤵ Offset interno da imagem" => "⤵ Inner offset of image",
        "⤵ Offset interno da seleção" => "⤵ Inner offset of selection",
        "📋 Copiar camada" => "📋 Copy layer",
        "📋 Copiar imagem" => "📋 Copy image",
        "📋 Copiar seleção" => "📋 Copy selection",
        "📎 Colar camada" => "📎 Paste layer",
        "📎 Colar imagem do clipboard" => "📎 Paste image from clipboard",
        "📎 Colar seleção" => "📎 Paste selection",
        "🔄 Espelhar H" => "🔄 Flip H",
        "🔄 Espelhar V" => "🔄 Flip V",
        "🔄 Espelhar horizontal" => "🔄 Flip horizontal",
        "🔄 Espelhar vertical" => "🔄 Flip vertical",
        "🔓 Desagrupar" => "🔓 Ungroup",
        "🔗 Agrupar" => "🔗 Group",
        "🖼 Arte" => "🖼 Artwork",
        "🖼 Rasterizar (criar traços de corte)" => "🖼 Rasterise (create cut paths)",
        "🗑 Apagar seleção" => "🗑 Delete selection",
        "🗑 Apagar vetorização" => "🗑 Delete trace",
        "🗑️ Apagar camada" => "🗑️ Delete layer",
        "🗑️ Apagar seleção" => "🗑️ Delete selection",
        "🗑️ Remover imagem" => "🗑️ Remove image",
        _ => pt,
    }
}
