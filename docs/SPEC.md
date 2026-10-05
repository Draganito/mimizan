# Mimizan Lab SPEC — Zahlen, die der Code braucht

Stand: Oktober 2026. Gilt zusammen mit `whitepaper_v1.pdf`. Wo dieses Dokument
vom Whitepaper abweicht, gilt dieses Dokument; jede Abweichung ist als
**[Abweichung]** markiert und begründet. Nichts hier ist "TBD". Jeder Default
hat einen Wert und eine Einheit.

Konventionen: `x` ist die Spalte (0 = links), `y` die Zeile (0 = oben).
`sx = (−1)^x`, `sy = (−1)^y`. Frequenzen in Perioden pro Pixel (c/px),
Nyquist = 0,5. Alle Bildwerte sind `f64`, normiert so dass Weißpegel = 1,0
und Schwarzpegel = 0,0.

---

## 1. Geltungsbereich

Eingang: Bayer-RAW mit 2×2-Muster (RGGB, BGGR, GRBG, GBRG), ganzzahlig
(≤ 16 Bit) oder Float, sowie monochrome DNGs (Photometric `LinearRaw` ohne
CFA). Ausgeschlossen (Fehler, kein Fallback): X-Trans (6×6), Quad-Bayer in
voller Auflösung (4×4), Foveon, Nikon High-Efficiency-NEF (HE/HE*), Dateien
ohne auswertbare CFA-Angabe. Sättigungsmaske, Rauschmodell, Trennung und
Mischung gelten pro Datei; Kameradatei (`cameras/*.json`) ist optional.

## 2. Einlesen

Dekoder: `rawler` 0.8 hinter dem Trait `RawDecoder`. Gelesen wird das rohe
CFA-Array (`RawImage.data`), nie ein demosaictes Bild.

| Größe | Quelle | Regel |
|---|---|---|
| Schwarzpegel | `blacklevel.levels`, positional über das CFA-Tile (`(y % h)·w + (x % w)`); ein einzelner Wert gilt für alle | pro Photosite abziehen |
| Weißpegel | `whitelevel.0`; ein Wert oder vier (RGBE-Reihenfolge nach Farbe) | pro Photosite teilen |
| CFA-Phase | `photometric = Cfa(cfg)`, `cfg.cfa.name` ∈ {RGGB, BGGR, GRBG, GBRG}, relativ zur vollen Sensorfläche (vor Crop) | Pflicht; Kameradatei darf nur bestätigen, nicht überschreiben |
| Crop | `crop_area` (Herstellerempfehlung); falls `None`: `active_area`; falls `None`: ganzes Bild | Crop-Ursprung muss gerade sein, sonst um 1 nach innen verschieben, damit die Phase erhalten bleibt |
| Monochrom | `photometric = LinearRaw` oder `BlackIsZero` mit `cpp = 1` | keine Trennung; `L = m` |

Normierung: `m = (raw − black) / (white − black)`. Werte unter 0 bleiben
negativ (Rauschen darf nicht geklippt werden), Werte über 1 bleiben über 1
bis zur Sättigungsregel.

**Sättigung.** Photosite gesättigt, wenn `raw ≥ white·(1 − sat_margin)` mit
`sat_margin = 0,02` (**[Abweichung]**: das Whitepaper sagt "8 Stufen", ohne
Einheit; bei 14 Bit wären 8 DN zu knapp, Sensoren werden Prozent vor dem
Weißpegel nichtlinear). Die Sättigungsmaske wird um 2 px dilatiert (die
gleichphasigen Nachbarn teilen den Schätzfehler). Gesättigte Photosites
bleiben mit ihrem geklippten Wert im Bild; sie werden in der Statistik
(Fits, Messungen) ausgeschlossen und in der Masken-Sidecar als 255 markiert.

**Defekte.** Für jede Photosite `v` die acht gleichphasigen Nachbarn
(Abstand 2 in x und/oder y), Median `med`, Minimum `lo`, Maximum `hi`.
Wenn `max(v − hi, lo − v) > max(8·σ(med), hi − lo)` (σ aus Abschnitt 3),
ersetze `v := med`. Die Nachbarspanne `hi − lo` ist ein modellfreier
Rauschmaßstab (≈ 3σ bei acht Nachbarn), damit die Regel auch bei ISO-Werten
oberhalb der Rauschdefaults hält. **[Abweichung]**: Das Whitepaper vergleicht nur gegen den
Median; an jeder scharfen Kante liegt ein Pixel mehr als 8σ vom
Nachbar-Median entfernt (gemessen: 8 % aller Photosites der Z f-Probe).
Ein Defekt liegt außerhalb des Nachbar-Wertebereichs, eine Kante nicht. Gesättigte Photosites und ihre
Nachbarn sind von der Defektregel ausgenommen. Die Defektkarte wird gezählt
und geloggt (Anzahl, Anteil, Zeilenhäufungen als PDAF-Hinweis).

**Kanalabgleich.** Nach Normierung und Defektregel wird jede Photosite mit
dem Multiplikator ihres Kanals `k_R, k_G = 1, k_B` multipliziert
(`--wb`): Default `as-shot` aus der Datei (`wb_coeffs`, auf G normiert),
Fallback und Option `gray` (Grauwelt: `k_c = mean_G / mean_c` über
ungesättigte Photosites), `none`, oder drei Zahlen. **[Abweichung,
gemessen]**: Das Whitepaper trennt auf Rohpegeln. Dann ist ein neutrales
Motiv in den Rohkanälen nicht neutral, die Chrominanz ist proportional zur
Luminanz (`C1, C2 ∝ L`) und das *gesamte* Luminanzspektrum sitzt als Kopie
auf den Trägern. Die Maske muss dann überall "Chrominanz" sagen; die
adaptive Stufe wird wirkungslos. Mit Abgleich sind die Träger neutraler
Motive leer, und die Maske kann Luminanz stehen lassen (Siemensstern:
MTF10 0,405 → 0,451 = Wahrheit, RESULTS.md). Alle folgenden Abschnitte
rechnen auf abgeglichenen Kanälen; die Gewichte in Abschnitt 6 beziehen
sich auf abgeglichene Kanäle (ein neutraler Grauwert gibt für jedes
Gewichtstripel denselben Ausgang, wie Filterfaktor-kompensierte
Schwarzweißfilter).

## 3. Rauschmodell

Standardabweichung in normierten Einheiten:

    σ(s) = noise_a · sqrt(max(s, 0)) + noise_b

**[Abweichung]**: Das Whitepaper nennt `noise_a = 0,005`, `noise_b = 10⁻⁵`
ohne Modellform. Als Varianzform wäre `a = 0,005` ISO-50 000-Niveau (SNR ≈ 6
bei Mittelgrau) und die Maske bliebe überall bei 0,5. Als
Standardabweichungsform passt `a = 0,005` zu einer 24-MP-Kamera bei
Basis-ISO (gemessen ≈ 28 DN bei 14 Bit, Modell ≈ 34 DN).

Defaults bis ein Dunkelbild vorliegt: `noise_a = 0,005`, `noise_b = 2·10⁻⁴`
(≈ 3 DN bei 14 Bit; das Whitepaper-`10⁻⁵` wäre 0,16 DN und damit unterhalb
jedes realen Ausleserauschens).

Das Modell gilt für Rohpegel. Nach dem Kanalabgleich hat eine Photosite der
Farbe c mit abgeglichenem Pegel `s` die Varianz `k_c²·σ²(s / k_c)`. Die
mittlere Varianz eines Blocks ist das Bayer-gewichtete Mittel
`¼·Var_R + ½·Var_G + ¼·Var_B` mit den *kanalweisen* Blockmitteln (ein
gemeinsames Blockmittel unterschätzt rote Blöcke um bis zu 15 %, gemessen).

## 4. Zerlegung

Mit `L = (R + 2G + B)/4`, `C1 = (−R + 2G − B)/4`, `C2 = (R − B)/4`
(Vorzeichen wie im Whitepaper) gilt für jede 2×2-Phase

    m(x, y) = L + ε·sx·sy·C1 + C2·(a·sx + b·sy)

| Phase (Farbe bei (0,0),(1,0),(0,1),(1,1)) | ε | a | b |
|---|---|---|---|
| RGGB | −1 | +1 | +1 |
| BGGR | −1 | −1 | −1 |
| GRBG | +1 | −1 | +1 |
| GBRG | +1 | +1 | −1 |

Herleitung: `p_R = (1 + r_x·sx)(1 + r_y·sy)/4` mit `r_x, r_y = ±1` nach der
Lage von R, analog B mit umgekehrten Vorzeichen; `p_G = 1 − p_R − p_B`.
Die Whitepaper-Formel `C2·[(−1)^x − (−1)^y]` ist der Fall GBRG.

Umkehrung (wird für die Mischung gebraucht):

    G = L + C1
    R = L − C1 + 2·C2
    B = L − C1 − 2·C2

Träger: C1 bei (0,5; 0,5); C2 bei (0,5; 0) und (0; 0,5). Chrominanz ist
physikalisch auf |f| ≤ 0,25 c/px um ihren Träger begrenzt.

## 5. Trennung

### 5.1 Demodulation und Tiefpass (feste Stufe)

    z1  = ε·sx·sy·m          Ĉ1  = LP(z1)
    z2a = a·sx·m             Ĉ2a = LP(z2a)
    z2b = b·sy·m             Ĉ2b = LP(z2b)

`LP` ist ein separabler Tiefpass (erst Zeilen, dann Spalten) mit identischem
1-D-Kern, Kaiser-gefensterter Sinc:

| Parameter | Default | Bedeutung |
|---|---|---|
| `chroma_cutoff` | 0,15 c/px | −6-dB-Grenze |
| `chroma_transition` | 0,05 c/px | Übergangsbreite (Durchlass bis 0,125, Sperre ab 0,175) |
| `chroma_attenuation` | 60 dB | Sperrdämpfung → Kaiser β = 0,1102·(A − 8,7) = 5,65 |
| Taps | aus den dreien berechnet, ungerade | N = ceil((A − 8)/(2,285·2π·Δf)) → 73 |

Rand: Spiegelung ohne Wiederholung des Randpixels ("reflect-101").
Der Kern wird so korrigiert (Konstante plus alternierende Konstante), dass
die Summe exakt 1 und die Antwort bei 0,5 c/px exakt 0 ist: ein flaches
Farbfeld trennt dann ohne jedes 2-px-Residuum (die Träger liegen bei
Nyquist). Das Durchlassband ist quadratisch
(|fx| < fc und |fy| < fc), nicht kreisförmig; die Ecken des Quadrats liegen
bei 0,21 c/px und damit noch innerhalb der physikalischen Chroma-Grenze.

**[Abweichung]**: Das Whitepaper sagt "volle FFT". Die Träger liegen fest,
also ist die Trennung eine Faltung mit festem Kern; im Ortsbereich ist sie
schneller, speicherärmer (keine komplexen 45-MP-Arrays in f64) und
deterministisch. Ergebnis ist identisch bis auf Rundung.

### 5.2 Blockmaske (`--mask adaptive`, nicht mehr Default, siehe 5.5)

Blöcke 64×64, Schritt 32 (50 % Überlapp), 2-D-Hann-Fenster, 2-D-FFT in
f64, Blockmittel vor dem Fenster abgezogen. Pro Block und Träger
k ∈ {C1, C2a, C2b} aus dem Blockspektrum von `m` (Supremumsnorm, passend
zum quadratischen Durchlassband):

    Kern   |f − f_k|_∞ < 0,05           immer Chrominanz (Farbton des Blocks, Farbkanten)
    Band   0,05 ≤ |f − f_k|_∞ < 0,15    strittig: P_C(k) = Σ |M(f)|²
    Ring   0,15 ≤ |f − f_k|_∞ < 0,35    Luminanz-Nachweis: P_L(k) = Σ |M(f)|²

Erwartete Rauschenergie pro Bin `N_bin = Var_Block·Σ w²` (Abschnitt 3,
kanalweise Blockmittel). Mit `n_C, n_L` Bins je Region:

    E_L = max(P_L − n_L·N_bin − 3·N_bin·sqrt(16·n_L), 0)
    E_C = max(P_C − n_C·N_bin − 3·N_bin·sqrt(8·n_C), 0)
    M_k = 0                                        falls E_L = 0  (kein Luminanz-Nachweis)
    M_k = 1                                        falls E_C = 0  (im Band nur Rauschen)
    M_k = min( min(1, 0,3·E_L / E_C)²,
               sqrt(n_C·N_bin / (E_C − 0,3·E_L)) )  sonst

`M_k` ∈ [0, 1]: 1 heißt "Bandinhalt ist Luminanz, stehen lassen", 0 heißt
"Bandinhalt ist Chrominanz, entfernen". Blöcke mit mehr als 25 %
gesättigten Photosites bekommen `M_k = 0` für alle k.

**[Abweichung, gemessen]** gegenüber `M = E_L/(E_L + E_C)`, 0,5 unter 2σ:

- *Kern immer entfernen.* Der Farbton eines Blocks und der Übergang an
  Farbkanten liegen unter 0,05 c/px um den Träger; dort gibt es keine
  Luminanz zu retten. Ohne diese Regel ließ jeder Block mit M > 0 den
  Farbton als 2-px-Gitter stehen.
- *Faktoren 16 und 8.* Die Summen über Bins eines Hann-gefensterten
  Bayer-Rauschspektrums streuen nicht wie unabhängige χ²-Bins
  (`sqrt(2·n)`), sondern 2,9× (Ring) bzw. 2× (Band) stärker: Fensterkorrelation,
  konjugierte Symmetrie und die 2×2-Untergitter (jedes Farbspektrum
  wiederholt sich mit Periode 0,5). Gemessen auf reinem Rauschen (Test
  `noise_sum_fluctuation_matches_constants`). Mit `sqrt(2·n)` sprach die
  Maske in flachen Feldern ständig an.
- *Leck-Verhältnis 0,3.* Luminanz mit 1/f²-Spektrum (Kanten) legt nur
  ∫₀,₃₅^0,5 f⁻² / ∫₀,₁₅^0,35 f⁻² ≈ 0,225 der Ringenergie ins Band; 0,3
  lässt Texturen etwas Spielraum. Was im Band darüber hinausgeht, ist
  Chrominanz.
- *Quadrat.* Zweifelsfälle (Farbkante mit Helligkeitskante) fallen auf den
  festen Schätzer zurück; eine Farbkante mit 16 % stehengelassener
  Bandchrominanz wäre ein sichtbares Gitter.
- *Deckel.* Das stehengelassene Residuum der nicht durch den Ring
  erklärbaren Bandenergie bleibt unter dem Rauschboden des Bandes:
  `M²·(E_C − leak) ≤ n_C·N_bin`.
- *Statt 0,5: 0.* Ohne Nachweis gilt der feste Schätzer (M = 0). 0,5 würde
  an jeder Farbkante die halbe Bandchrominanz stehen lassen.
- *Erosion.* Vor der Interpolation wird das Blockgitter mit einem
  3×3-Minimum erodiert: ein Block, der "Chrominanz" sagt, gewinnt gegen
  seine Nachbarn, damit die bilineare Interpolation kein M > 0 in die 32 px
  neben einer Farbkante trägt (gemessen: Farbfeldtafel, Trägerenergie 96×
  Rauschen → 0,8× nach Erosion).

Blockwerte sitzen auf den Blockmitten und werden bilinear auf Pixel
interpoliert; außerhalb der äußersten Blockmitten wird geklemmt. Die
Blockanalyse ist reine Analyse (das Fenster wird einmal angewendet, Hann mit
50 % Überlapp ist COLA); es gibt keine Block-Synthese und damit keine
Blockrand-Artefakte. Bilder unter 64 px in einer Richtung laufen mit dem
festen Schätzer.

Zwei C2-Kopien (Dubois): `ρ_a = 1 − M_2a`, `ρ_b = 1 − M_2b`,

    Ĉ2' = (ρ_a·Ĉ2a' + ρ_b·Ĉ2b') / (ρ_a + ρ_b)      falls ρ_a + ρ_b > 10⁻⁹
        = (Ĉ2a' + Ĉ2b') / 2                        sonst (beide voll maskiert)
    g2 = max(ρ_a, ρ_b)
    g1 = 1 − M_1

Die Kombination gilt für die bereits nach 5.3 zerlegten Schätzungen `Ĉ'`.
**[Abweichung, gemessen]**: Eine frühere Fassung mit `+10⁻⁶` im Nenner
ließ bei `M_2a = M_2b = 1` den Kern beider Kopien verschwinden
(`0/10⁻⁶ = 0`), d. h. in texturierten Flächen mit Farbstich blieb der
Chroma-Gleichanteil als 2-px-Gitter in `L̂` (sichtbar im Z-f-Testbild auf
dem Stich auf getöntem Papier). Mit dem Mittelwert bleibt der Kern immer
Chroma, wie 5.3 fordert.

### 5.3 Luminanz

Jede Chrominanzschätzung wird in Kern und Band zerlegt. Der Kern ist der
Tiefpass von `Ĉ` auf 0,05 c/px (Kaiser-Sinc wie 5.1, Übergang 0,05 c/px,
gerechnet auf halber Auflösung, da `Ĉ` unter 0,175 c/px bandbegrenzt ist,
und bilinear zurück):

    Ĉ_k' = g_k·Ĉ_k + (1 − g_k)·Kern(Ĉ_k)
    L̂ = m − ε·sx·sy·Ĉ1' − (a·sx + b·sy)·Ĉ2'

(Die Form `g·Ĉ + (1−g)·Kern` ist bei `g = 1` und `g = 0` exakt, so dass
der adaptive Pfad auf Flächen ohne Maske bitgleich zum festen Schätzer
ist.)

Ohne adaptive Stufe (`--mask off`) gilt `g1 = g2 = 1`, also `Ĉ' = Ĉ`, und
`Ĉ2 = (Ĉ2a + Ĉ2b)/2`.

### 5.4 Konsistenzschleife

**[Abweichung]**: Das Whitepaper nennt POCS. Eine Projektion auf die
Messwerte würde das Mosaik in `L̂` zurückschreiben, was das Whitepaper selbst
verbietet. Was bleibt, ist die Kreuzterm-Bereinigung zwischen den Trägern
(Gauss-Seidel): Vor der Demodulation von Träger k werden die aktuell
geschätzten Beiträge der anderen Träger abgezogen.

    Runde n:
      für k in (C1, C2a, C2b):
        m_k = m − Σ_{j≠k} g_j·carrier_j·Ĉ_j        (mit den aktuellsten Ĉ_j)
        Ĉ_k = LP(carrier_k · m_k)
      L̂_n nach 5.3
    Abbruch, wenn eine Bedingung erfüllt ist:
      (a) ‖L̂_n − L̂_{n−1}‖₂ / ‖L̂_{n−1}‖₂ < 10⁻⁴
      (b) Trägerenergie von L̂_n (Abschnitt 8) unter der erwarteten Rauschenergie
      (c) n = 12

Default `consistency_rounds_max = 1` (**[gemessen]**: auf der Messbank
änderte keine weitere Runde eine Kennzahl jenseits der vierten Stelle,
während sich die Laufzeit verdoppelte; `--rounds` bleibt als Option). Die
Maske wird in der Schleife nicht neu berechnet.

### 5.5 Pixeladaptive Trennung (`--mask dubois`, Default)

**[Abweichung, gemessen]** gegenüber 5.1–5.3 (Kodak-Messbank, RESULTS.md
letzter Abschnitt): Die feste Stufe mittelt die beiden C2-Kopien, obwohl
Luminanz immer nur in eine von ihnen leckt (vertikale Linienmuster in die
Kopie bei (0,5, 0), horizontale in die bei (0, 0,5)); die Blockmaske kann
ein Linienmuster innerhalb des Bandes nicht von Chrominanz unterscheiden
(gleiches Band/Ring-Verhältnis) und reduziert sich nach der Erosion fast
überall auf das Mittel; und das quadratische Band verschenkt entlang des
Trägers Chroma-Bandbreite, wo der einzige Nachbar die andere
Chroma-Komponente ist. Ersatz nach Dubois (2005), auf C1 erweitert:

Zwei Hypothesen mit separablen Kaiser-Sinc-Tiefpässen (Kerne wie 5.1,
`LP_n` schmal, `LP_w` breit; Zeilen × Spalten):

    H (horizontale Struktur, Spektrum entlang v; Kopie a sauber):
      Ĉ2a = LP_n,x LP_w,y (a·sx·m)       Ĉ1_H = LP_n,x LP_w,y (ε·sx·sy·m)
    V (vertikale Struktur, Spektrum entlang u; Kopie b sauber):
      Ĉ2b = LP_w,x LP_n,y (b·sy·m)       Ĉ1_V = LP_w,x LP_n,y (ε·sx·sy·m)

Lokale Energien und Gewicht pro Pixel:

    e_a = G_σ * Ĉ2a²      e_b = G_σ * Ĉ2b²
    w_H = e_b / (e_a + e_b)        (0,5 falls e_a + e_b ≤ 10⁻¹⁸)
    Ĉ2 = w_H·Ĉ2a + (1 − w_H)·Ĉ2b
    Ĉ1 = w_H·Ĉ1_H + (1 − w_H)·Ĉ1_V
    L̂ = m − ε·sx·sy·Ĉ1 − (a·sx + b·sy)·Ĉ2

Die Kopie mit der kleineren lokalen Energie ist die vertrauenswürdige: die
Chrominanz steht in beiden, der Überschuss ist Luminanz. Dasselbe Gewicht
wählt die Richtung des C1-Bandes.

**Kohärenzring** (seit 0.3.0, **[gemessen]**, RESULTS „Coherence ring“):
Das längliche Band bezahlt überall mit Chroma-Bandbreite, auch wo keine
Luminanz nahe dem Träger liegt. Wo der Ring zwischen dem schmalen
quadratischen Band (`c2_narrow`) und einem breiteren quadratischen Band
(`ring_wide`) in beiden Kopien denselben Inhalt trägt, ist er Chrominanz
(Luminanz leckt nur in eine Kopie) und das Band darf auf die quadratische
breite Form geöffnet werden. Mit `LP_r` dem quadratischen Tiefpass bei
`ring_wide`, vor der Gewichtung der Kopien:

    rA = LP_r,x LP_r,y (a·sx·m) − LP_n,x LP_n,y (a·sx·m)       (Kopie a, quadratisch breit minus schmal)
    rB = LP_r,x LP_r,y (b·sy·m) − LP_n,x LP_n,y (b·sy·m)
    κ  = clamp( G_σ*(rA·rB) / sqrt( G_σ*rA² · G_σ*rB² ), 0, 1 )^ring_power     (0 falls Nenner ≤ 10⁻¹⁸)
    Ĉ2a := Ĉ2a + κ·(LP_r,x LP_r,y (a·sx·m) − Ĉ2a)       ebenso Ĉ2b; danach die Mischung mit w_H
    Ĉ1  := Ĉ1  + κ·(LP_r,x LP_r,y (ε·sx·sy·m) − Ĉ1)     falls ring_c1, nach der Richtungsmischung

κ ist die normierte Kreuzkorrelation der beiden Ringe im Fenster G_σ:
1 bei identischem Inhalt, 0 bei unkorreliertem, negative Korrelation wird
zu 0. `ring_wide` = 0 schaltet den Schritt ab (Ergebnis von 0.2.0).

| Parameter | Default | Bedeutung |
|---|---|---|
| `c1_narrow` | 0,15 c/px | C1 quer zur Strukturrichtung |
| `c1_wide` | 0,30 c/px | C1 entlang der Strukturrichtung |
| `c2_narrow` | 0,15 c/px | C2 quer zum Träger (zur Luminanz hin) |
| `c2_wide` | 0,30 c/px | C2 entlang des Trägers |
| `transition` | 0,15 c/px | Übergangsbreite |
| `attenuation_db` | 40 dB | Sperrdämpfung |
| `energy_sigma` | 3 px | σ des Gauß-Fensters der lokalen Energie und der Kohärenz |
| `ring_wide` | 0,25 c/px | quadratisches breites Band des Kohärenzrings; 0 = aus |
| `ring_power` | 2 | Exponent auf κ |
| `ring_c1` | wahr | auch C1 mit κ öffnen |

Grenze des breiten Bandes: Unter V wollen C1 (0,5, 0,5) und Ĉ2b (0, 0,5)
beide entlang derselben Linie v = 0,5 breit sein und treffen sich bei
u = 0,25; mit Übergang 0,15 überlappen sie ab 0,35 (gemessener Einbruch
bei 0,40). Dieselbe Grenze gilt für `ring_wide`: 0,30 verliert in jeder
Bedingung, 0,20 liegt noch im Übergangsband des schmalen Filters und misst
nichts. Flaches Farbfeld: exakt (Test `flat_colour_is_exact`); neutrales
vertikales Linienmuster bei 0,42 c/px: unter 10 % des Fehlers der festen
Stufe (Test `neutral_vertical_lines_survive`). Die Maske nach 5.2 wird in
diesem Modus nicht berechnet; `mask_max` ist konstant 0,8 wie bei
`--mask off` (Abschnitt 7), 5.3 (Kern/Band-Zerlegung) und 5.4 entfallen.

Laufzeit Trennung 2,17 s auf 24,4 MP (ohne Kohärenzring 1,12 s, fest
0,75 s, Blockmaske 1,16 s), gesamt 3,3 s — **[Abweichung]** vom Budget in
Abschnitt 12. Speicher: Spitze zehn volle f64-Ebenen einschließlich Mosaik
(2,0 GB gemessen; sieben ohne Kohärenzring): die Energie- und
Kohärenz-Tiefpässe laufen in den Puffern ihrer Operanden mit einer
Scratch-Ebene für den Zeilenpass, die Zeilenpässe des Mosaiks teilen eine
Ebene.

### 5.6 Nichtlineare Rekonstruktion (`--reconstruct`, Checkbox, Default aus)

**[Erweiterung, gemessen]** (RESULTS „Reconstruct“). Alles bis hier ist
linear: jedes Ausgabepixel ist eine Summe gemessener Werte mit Gewichten,
die nicht vom Bildinhalt an dieser Stelle abhängen (5.5 wählt zwischen zwei
solchen Summen, erfindet aber keine). Die Grenze dieses Weges ist die
letzte Oktave: wo Luminanz nahe den Trägern liegt, kann kein Filter sie
von Chrominanz trennen. Demosaicer lösen das mit einer Annahme — die
Farbdifferenzen `G − R`, `G − B` sind lokal glatt — und einer
Richtungsentscheidung pro Pixel. Dieser Schritt tut dasselbe, nur dort, wo
die lineare Schätzung noch Energie nahe den Trägern trägt, und schreibt
pro Pixel in die Sidecar, wie viel davon berechnet statt gemessen ist.

Schätzer (Zhang & Wu 2005, aus dem Papier geschrieben), im Arbeitsbereich
`m^(1/γ)` mit `γ = 2,2`:

    Richtungsschätzung des fehlenden Kanals entlang x (ebenso y), an jedem Pixel:
      X̂(x) = (0,5 + A)·(X(x−1) + X(x+1)) − A·(X(x−3) + X(x+3)) + LAP·(2·O(x) − O(x−2) − O(x+2))
      A = 0,0625, LAP = 0,125 (Hamilton–Adams ist A = 0, LAP = 0,25); O der eigene Kanal
    Farbdifferenzsignale  d_h = G − X entlang der Zeile,  d_v entlang der Spalte
    LMMSE je Linie: s = d * [4 9 15 23 26 23 15 9 4]/128 (Prior),
      im Fenster ±4:  v_s = Var(s),  v_n = Mean((d − s)²)
      d̂ = s + v_s/(v_s + v_n)·(d − s),   Fehler e = v_s·v_n/(v_s + v_n)
    Fusion an den R/B-Stellen:  w_h = 1/(e_h + 10⁻¹² + μ·d̂_h²),  w_v analog, μ = 0,01
      Ĝ = X + (w_h·d̂_h + w_v·d̂_v)/(w_h + w_v)
    R − G und B − G: an der Gegenstelle Mittel der vier Diagonalen, an G-Stellen
      Mittel der zwei Nachbarn, die den Kanal tragen
    Rücktransformation ^γ, dann L = (R + 2G + B)/4, C1 = (2G − R − B)/4, C2 = (R − B)/4

μ ist der Zünglein-Term: wo beide Richtungen in sich konsistent sind
(neutrales Nyquist-Muster, Fehler beidseits null), gewinnt die mit weniger
Chrominanz (Test `neutral_nyquist_lines_are_exact`); flaches Farbfeld
exakt (`flat_colour_is_exact`). Der Arbeitsbereich `γ = 2,2` ist **der**
Unterschied zur Literatur, die 8-Bit-sRGB-Bilder direkt demosaict: in
linearem Licht sind Farbdifferenzen über eine Kante nicht konstant,
komprimiert fast (1×-Teilmenge: 36,6 → 42,2 dB, real binned 37,5 → 38,5).

Detektor (`Residue`): die lineare Luminanz L̂ aus 5.5 wird mit den drei
Trägern demoduliert und mit `G_σ` (σ = 2 px) tiefpassgefiltert; die Summe
der Amplitudenquadrate gegen die Rauschvarianz im selben Band:

    r = Σ_c ( G_σ * (c·L̂) )²  /  ( σ²(L̂) · ‖G_σ‖⁴ )
    β = clamp( (r − lo)/(hi − lo), 0, 1 ),   lo = 20, hi = 80
    L̂ := L̂ + β·(L_n − L̂),  Ĉ1, Ĉ2 ebenso;  mask_max := 0,8 + 0,19·β

Wo die lineare Trennung sauber war, liegt nahe den Trägern nur Rauschen
(r ≈ 1) und β = 0: das Pixel bleibt gemessen. Sidecar (§7): 204 = gemessen,
252 = ganz berechnet, dazwischen linear; 255 bleibt Sättigung, die
Schärfungsschwelle 0,8 bleibt erfüllt.

| Parameter | Default | Bedeutung |
|---|---|---|
| `detector` | `Residue` | `All` setzt β = 1 überall (Messzwecke) |
| `lo`, `hi` | 20, 80 | Rampe von β in Einheiten Energie/Rauschvarianz |
| `sigma` | 2 px | Fenster des Detektors |
| `mu` | 0,01 | Chroma-Zünglein der Richtungsfusion |
| `gamma` | 2,2 | Arbeitsbereich des Schätzers; 1,0 = linear |

Verworfen, gemessen: Detektor auf der Differenz der beiden
Luminanzschätzungen (schlechter in jeder Bedingung), auf dem lokalen
Gradienten (nutzlos), als skalenfreier Anteil Trägerband/AC-Energie
(unterscheidet nicht zwischen Signal und Rauschen im Trägerband; −3 dB bei
1×); 2-D-Fehlerstatistik über die Nachbarlinien (−0,2 dB); Glättung der
Farbdifferenzen als Verfeinerungsschritt (−0,7 bis −1,3 dB); längere
Richtungsfilter per 1-D-Demodulation (gleich bis −0,2 dB mit LMMSE,
katastrophal ohne). Die Bedingung „band-limitiert mit echtem Rauschen"
(`--raw --scale 2`) ist der Grund für den Detektor: dort ist der
nichtlineare Schätzer allein 6 dB schlechter als 5.5 (die glatte Prior
nimmt bandbegrenztem Chroma seinen Inhalt), mit Detektor gleich
(48,56 gegen 48,57).

Laufzeit: +2,2 s auf 24,4 MP (Trennung 4,5 s statt 2,3 s), Spitze vier
zusätzliche Ebenen. Das Negativ-JSON (§7) trägt `reconstruct` (wahr/falsch)
und `computed` (mittleres β).

## 6. Mischung

Drei Gewichte `w_R, w_G, w_B ≥ 0`, Summe 1, auf *abgeglichene* Kanäle
(Abschnitt 2). Default 0,25 / 0,50 / 0,25.

    out = L̂ + (w_G − w_R − w_B)·Ĉ1' + 2·(w_R − w_B)·Ĉ2'

(`Ĉ'` aus 5.3; die Gewichte g sind darin bereits enthalten.) Für die
Matrix-Variante ist `w` die Y-Zeile von `cam_to_xyz`, kanalweise durch
`k_c` geteilt und auf Summe 1 normiert.

**Gewichtsraum [Ergänzung, gemessen].** Die Formel verbraucht Gewichte auf
abgeglichenen Kanälen (`balanced`). Eine Monochromkamera hat aber eine
feste Spektralantwort und kennt keinen Abgleich; ihre Nachbildung ist ein
festes Tripel auf den *rohen* Kanälen (`raw`). Beide Räume hängen exakt
zusammen:

    w_bal,c ∝ w_raw,c / k_c        w_raw,c ∝ w_bal,c · k_c      (jeweils auf Summe 1 normiert)

Die Kameradatei speichert gefittete Gewichte im Rohraum
(`weights_space = "raw"`); die Entwicklung rechnet sie pro Bild mit den
Multiplikatoren `k_c` des Abgleichs in den abgeglichenen Raum um und
mischt dann wie oben. Das Negativ trägt beide Formen in der Beschreibung
(`weights`, `weights_raw`). Begründung (RESULTS.md): dieselbe Szene unter
Tageslicht und Kunstlicht — die Z-f-Gewichte im abgeglichenen Raum springen
von (0,171/0,489/0,340) auf (0,297/0,550/0,153), weil die
As-shot-Multiplikatoren (R 2,02→1,26, B 1,11→2,05) in sie hineingefaltet
sind; im Rohraum bleiben sie bei (0,284/0,404/0,312) gegen
(0,302/0,444/0,254). Ohne Angabe gilt `balanced` (ältere Dateien,
Default-Tripel, CLI-Override `--weights`).

**[Abweichung/Präzisierung]**: Das Whitepaper sagt "C bleibt aussen" und
fordert gleichzeitig drei Gewichte. Beides zugleich geht nur für
0,25/0,50/0,25 (dann ist `out = L̂` exakt). Jedes andere Gewicht braucht die
tiefpassgefilterte Chrominanz, siehe Umkehrung in Abschnitt 4. Die
Chrominanz-Korrektur ist bandbegrenzt (≤ 0,15 c/px), die Kantenauflösung
bleibt die von `L̂`. `matrix_fallback = true` heißt: `w` ist die normierte
Y-Zeile der Kameramatrix (`rgb_cam`), ebenfalls erst nach der Trennung.

**Kontrastfilter [Ergänzung].** Ein Farbfilter vor einem Monochromsensor
ist eine spektrale Gewichtung vor der Summe, also genau das, was die
Mischung tut. Ein Filter mit Kanaltransmissionen `T_R, T_G, T_B` auf den
abgeglichenen Kanälen macht aus den Basisgewichten

    w'_c = w_c · T_c / Σ_j w_j · T_j

Der Filterfaktor (Lichtverlust) fällt durch die Normierung weg; digital
kostet er keine Belichtung. Basis sind die Gewichte der Kameradatei (ohne
Datei: nativ). Die Pipeline ändert sich nicht, ein Filter ist nur ein
anderes Tripel; `filter` steht in der Negativbeschreibung. Tabelle
(`ColorFilter`), Transmissionen als *Näherungen* typischer Filterkurven
über typische Kanalbänder, nicht gemessen:

| Filter | Wratten | T_R | T_G | T_B |
|---|---|---|---|---|
| yellow-8 | 8 (K2) | 1,00 | 0,90 | 0,10 |
| yellow-green-11 | 11 | 0,55 | 1,00 | 0,25 |
| orange-16 | 16 | 1,00 | 0,50 | 0,02 |
| red-25 | 25 | 1,00 | 0,08 | 0,00 |
| green-58 | 58 | 0,10 | 1,00 | 0,10 |
| blue-47 | 47 | 0,03 | 0,25 | 1,00 |

Qualität (RESULTS.md, Z f): Auflösung und Pixelrauschen bleiben bei jeder
Gewichtung die von `L̂`; der Filter fügt nur bandbegrenztes Chromarauschen
(≤ 0,15 c/px) hinzu, beim Rotfilter 19–21 % σ auf glatten Flächen bei
unverändertem Pixel-zu-Pixel-Rauschen. Eine gemessene Variante würde wie
die Monochromgewichte gefittet: Referenzkamera plus physischer Filter plus
Farbtafel (`calibrate weights`).

## 7. Negativ-Datei

16-Bit-TIFF, ein Kanal, `PhotometricInterpretation = BlackIsZero`,
`value = round(clamp(out, 0, 1)·65535)`. Eingebettetes ICC-Profil: Grau,
D50-Weißpunkt, `kTRC` = Gamma 1,0 (lineares Grau). `ImageDescription` enthält
JSON mit Kamera, Phase, Abgleich (`wb`), Gewichten in beiden Räumen
(`weights` abgeglichen, `weights_raw` roh, §6), Maskenmodus, Rundenzahl,
Rauschparametern, Programmversion. Orientierung aus dem RAW wird als TIFF-`Orientation`
übernommen, die Pixel werden nicht gedreht. Quelle ist das Raw-IFD; meldet
es nichts Gedrehtes, gilt die EXIF-`Orientation` (die Z f schreibt sie nur
dort). Die Aufnahmezeit (`DateTimeOriginal`) steht als `captured` bei der
Belichtung.

Sidecar `<name>.mask.tif`: 8-Bit, `round(255·max(M_1, M_2a, M_2b))`,
gesättigte Photosites (dilatiert) überschreiben mit 255. Wird vom Druck für
die Schärfungsschwelle (§9) gelesen. Ohne Blockmaske (`--mask off`, der
Default `--mask dubois`) und bei Monochromsensoren trägt die Sidecar
überall 204 (= Schwelle 0,8, „Schärfung erlaubt": die Träger sind durch den
60-dB-Tiefpass in jedem Block unterdrückt, RESULTS Phase 3) und 255 an
gesättigten Photosites. Mit `--reconstruct` (5.6) trägt sie
`round(255·(0,8 + 0,19·β))`: 204 = gemessen, 252 = ganz berechnet; das JSON
trägt `reconstruct: true` und `computed` (mittleres β).

## 8. Messgrößen

- **Trägerenergie** eines Bildes: Energie des 2-D-Spektrums (blockweise wie
  5.2, über alle Blöcke gemittelt) in `|f − f_k|_∞ < 0,15` für die drei
  Träger, relativ zur erwarteten Rauschenergie derselben Bins.
  Bestehensgrenze auf Farbflächen: ≤ 1,5.
- **Graukeil-Linearität**: Patch-Mittel gegen relative Belichtung, Fit in
  log-log; Bestehen bei Steigung 1,00 ± 0,02 und maximalem relativem
  Residuum ≤ 2 %. **[Abweichung]**: R² ≥ 0,98 bestehen auch Gamma 1,2 und
  Toes; es ist kein Linearitätstest.
- **MTF am Siemensstern**: Kontrast gegen Ortsfrequenz getrennt für
  achsparallel (±10° um 0°/90°) und diagonal (±10° um 45°/135°); MTF50 und
  MTF10 in c/px; Aliasing-Ring = Kontrastanstieg jenseits des ersten
  Minimums.
- **RMSE gegen Wahrheit** (nur synthetisch): `L_true = (R + 2G + B)/4` der
  erzeugten Szene, RMSE und 99-Perzentil des Fehlers, ohne 40-px-Rand.

Kein Prozentwert ohne eine dieser Messungen.

## 9. Ausgang (Druck und Bildschirm)

Reihenfolge: Mischung → Größe → Kurve → [Entfaltung →] USM *oder*
Bildschirmkompensation → Datei.

- **Orientierung**: Das Negativ trägt die RAW-Orientierung nur als Tag
  (§7). Der Druck dreht zuerst Pixel und Sidecar-Maske aufrecht
  (Orientierung 1–8), danach steht im Druck-TIFF `Orientation = 1`.
- **Größe**: Ziel `--size <B>x<H><cm|mm|in>` plus `--dpi`; Zielpixel =
  Zoll·DPI, Bild wird eingepasst (Seitenverhältnis bleibt). Die Angabe
  ist das Papier, nicht die Bildlage: Ein Querformat auf „30x40cm“ wird
  in 40×30 cm eingepasst. Lanczos-3 separabel in f64; beim Verkleinern
  wird der Kern mit dem Skalenfaktor gestreckt (das ist der Tiefpass
  vorher), Ränder gefaltet (Gewichte außerhalb werden auf den Randpixel
  gelegt, Summe exakt 1). Niemals über Zielpixel hinaus skalieren. Die
  Maske wird mit demselben Kern mitskaliert. Ohne `--size` bleibt die
  Pixelzahl des Negativs. Alternativ `--size <N>px`: lange Kante in
  Pixeln (Einpassen in ein Quadrat N×N), für den Bildschirm.
- **Kurve**: `look/*.json` mit monotonen Stützstellen in [0,1]² (Eingang
  linear, Ausgang kodiert), interpoliert mit PCHIP (monoton). Preset
  `neutral`: reine Gamma-2,2-Kodierung. Preset `reference`: durch
  `calibrate wedge` aus dem Graukeil der Referenz-Monochromkamera (lineares
  DNG gegen ihr eigenes Kamera-JPEG) gefittet, RESULTS.md. Druck-TIFF
  bekommt ein Grau-ICC mit Gamma 2,2.
- **USM**: Radius in Pixel

      r_px = usm_k · d_mm · DPI,   usm_k = 1,15·10⁻⁵ (Default)

  Herleitung: 1 Bogenminute = 2,9·10⁻⁴ rad; 2,9·10⁻⁴ / 25,4 mm/in =
  1,15·10⁻⁵ px pro (mm·DPI). Beispiel 400 mm, 300 DPI → 1,4 px.
  **[Abweichung]**: Die Whitepaper-Formel `(d·DPI·4)/10 000` ergibt 48 px
  für dasselbe Beispiel. `d_mm` kommt aus `--distance-mm`, sonst die
  Druckdiagonale. `usm_amount` ∈ [0; 1,5]; Default aus der Kameradatei,
  die im Negativ genannt ist, sonst 0. USM nur dort, wo die
  Sidecar-Maske ≥ 0,8 (204/255) **und < 1,0** ist: 255 markiert
  gesättigte Photosites, dort darf kein Halo entstehen. Ohne Sidecar wird
  USM mit Warnung überall angewendet. Gauß-Kern mit σ = r_px, Länge
  2·⌈3σ⌉+1, Rand reflect-101. Die USM arbeitet auf dem kodierten Wert
  (nach der Kurve), weil der Schärfeeindruck wahrnehmungsbezogen ist.
- **Entfaltung [Ergänzung]** (`--deconv <N>`, N ∈ [1; 10], aus ohne Flag):
  Richardson–Lucy mit Gauß-PSF, σ = r_px (derselbe Radius wie die USM),

      e_{k+1} = e_k · G ⊛ ( b / (G ⊛ e_k) ),   e_0 = max(b, 0)

  N Durchgänge, auf dem kodierten Wert vor der USM, auf **jedem** Pixel —
  die Sidecar-Maske greift hier nicht, weil die Inverse keinen Halo
  aufbaut, sondern die Unschärfe zurückrechnet; dafür bringt sie das
  Rauschen zurück, das die Unschärfe genommen hat. Die PSF ist nicht
  gemessen, sondern die Annahme „Betrachter-Unschärfe = USM-Radius“;
  der Schritt ist damit eine Schärfung durch Inversion, keine Restaurierung.
  Eine USM kann danach noch folgen (`--usm-amount`). Schließt
  `--screen` aus. Die Dateibeschreibung nennt `deconvolution_iterations`,
  solange > 0.
- **Bildschirmkompensation [Ergänzung]** (`--screen <amount>`, ersetzt die
  USM): Für ein Bild, das 1:1 auf einem Bildschirm steht, sind die
  Verluste bekannt und nicht vom Betrachter abhängig: die Lanczos-3-
  Verkleinerung mit MTF `L(f)` (Fourier-Transformierte des auf 1
  normierten Kerns in Ausgabepixeln; 1,01 bei 0,2 c/px, 0,99 bei 0,3,
  0,82 bei 0,4, 0,50 bei 0,5) und die Pixelapertur des Displays
  (100 % Füllfaktor, `A(f) = sinc(f) = sin(πf)/(πf)`, 0,637 bei Nyquist).
  Bekannte Übertragung `M(f) = L(f)·A(f)` (0,985 / 0,947 / 0,849 / 0,623
  bei 0,1 / 0,2 / 0,3 / 0,4 c/px); ohne Verkleinerung nur `A(f)`.
  Ziel ist die regularisierte Inverse (Wiener)

      T(f) = (1+ε) · M(f) / (M(f)² + ε),   ε = 1 / (4·g_max²),  g_max = 2 (Default)

  so dass `T(0) = 1` und die Verstärkung nahe Nyquist auf etwa `g_max`
  begrenzt bleibt (Rauschen). `T` wird durch einen symmetrischen 9-Tap-FIR
  `H(f) = h₀ + 2·Σ h_k cos(2πkf)` im Sinne kleinster Quadrate über
  `f ∈ [0; 0,5]` angenähert, Gewicht `1/(f + 0,1)` (natürliche Bilder haben
  ~1/f-Amplitudenspektren, der Fehler soll dort klein sein, wo das Bild
  Energie hat), danach exakt auf DC-Verstärkung 1 normiert. Ergebnis für
  Verkleinerung: `H·M` = 0,98/1,00/0,99/0,95/0,94 bei 0,1/0,2/0,25/0,3/0,4
  c/px, `H(0,5)` = 1,91. `amount ∈ [0; 1]` mischt den Kern linear mit der
  Identität; mehr als 1 wird nicht angeboten, weil es die gemessenen
  Verluste überkompensieren würde. Ein Gauß-USM kann diese Inverse nicht
  darstellen (ein Fit läuft auf σ < 0,3 px). Maskenregel wie bei der USM,
  separabel, Rand reflect-101, nach der Kurve. Nicht bekannt und daher
  nicht kompensiert: ein Browser, der das Bild selbst skaliert (HiDPI-Zoom),
  Betrachtungsabstand, Display-ppi.
- **Datei**: 16-Bit-TIFF mit Kurve und Gamma-2,2-Grau-ICC (bei
  `encoding = none` lineares ICC), `XResolution/YResolution` = DPI,
  `ImageDescription` = JSON mit Quelle, Look, Größe, USM- bzw.
  Kompensationsparametern (Taps, Antwort bei 0,25 und 0,40 c/px) und
  der Beschreibung des Negativs. Endet `-o` auf `.jpg`/`.jpeg`, wird nur
  das JPEG geschrieben (Bildschirm). `--proof` schreibt zusätzlich ein
  8-Bit-Grau-JPEG derselben Pixel in voller Druckauflösung, Qualität 100
  (ein Kanal, kein Chroma-Subsampling), mit demselben Grau-ICC wie das
  TIFF.

## 10. Eichdateien

`cameras/<clean_make>_<clean_model>.json` (Kleinbuchstaben, Leerzeichen →
`_`):

```json
{
  "schema": 1,
  "make": "Nikon", "model": "Z f",
  "bayer_phase": "RGGB",
  "noise_a": 0.005, "noise_b": 0.0002,
  "weights_rgb": [0.25, 0.50, 0.25],
  "weights_space": "balanced",
  "usm_amount": 0.0,
  "matrix_fallback": false,
  "fitted": { "noise": false, "weights": false }
}
```

Regeln: `bayer_phase` muss zur Datei passen, sonst Fehler. `weights_rgb`
nichtnegativ, Summe 1 ± 10⁻⁶. `weights_space` ist `balanced` (Default,
Tripel wird direkt gemischt) oder `raw` (Tripel auf Sensorkanälen,
Umrechnung pro Bild nach §6; so schreibt `calibrate weights`). Fehlt die
Datei: Defaults wie oben. Schwarz-
und Weißpegel kommen immer aus der RAW-Datei, nie aus der Kameradatei.
Optionale Felder, von `calibrate` geschrieben: `weights_fitted_against`
(Referenzdatei des Gewichte-Fits) und `star` (`mtf50_axis`, `mtf50_diag`,
`mtf10_axis`, `mtf10_diag` in c/px, `ring`, `source`).

### Eichverfahren (`mimizan calibrate`)

- **`wedge`**: Graukeil als Negativ (linear) und als Referenz-JPEG der
  Zielkamera. Pro Feld (Rechtecke oder Raster `x,y,w,h,cols,rows` mit
  Einzug) Mittelwert im Negativ `n_i` und in der Referenz `r_i` (8-Bit
  dekodiert, /255). Stützstellen `(n_i^(1/2,2), r_i)` bei `gamma22`,
  sortiert, Felder mit `|Δx| < 10⁻³` gemittelt, Monotonie durch
  gewichtete isotone Regression (Pool-Adjacent-Violators) erzwungen,
  Anker (0,0) und (1,1) ergänzt. Felder mit > 1 % gesättigten Photosites
  (Sidecar = 255) werden verworfen. Bestehen: ≤ 3/255 auf den Feldern;
  der synthetische Selbsttest misst zusätzlich zwischen den Stufen
  (Hold-out) und die Linearität nach §8.
- **`weights`**: Farbtafel als RAW und als Referenz (JPEG der Zielkamera,
  sRGB-dekodiert zu linear, oder Werteliste). Pro Feld balancierte Mittel
  `(R_i, G_i, B_i)` ohne gesättigte Photosites. Modell
  `s · (w_R R_i + w_G G_i + w_B B_i) ≈ ref_i`, `s` frei (Belichtung),
  `w ≥ 0`, `Σw = 1`. Suche erschöpfend auf dem Simplex (Schritt 0,01,
  verfeinert 0,001) — deterministisch, keine lokalen Minima. Bestehen:
  RMSE ≤ 5 % der mittleren Referenzhelligkeit (§11). Der Fit läuft im
  abgeglichenen Raum des Aufnahmebalance (`--wb`); `--write` rechnet das
  Ergebnis mit den Multiplikatoren dieser Aufnahme in den Rohraum um und
  trägt es als `weights_space = "raw"` in die Kameradatei ein
  (`fitted.weights = true`). Mit `--wb none` ist der Fit bereits im
  Rohraum.
- **`star`**: MTF nach §8 auf einem Negativ, mit `--write` in `star` der
  Kameradatei; optional `--usm-amount` setzen. Mittelpunkt und Radien
  werden angegeben, nicht gesucht.

`look/<name>.json`:

```json
{ "schema": 1, "name": "neutral", "points": [[0.0, 0.0], [1.0, 1.0]], "encoding": "gamma22", "fitted_against": null }
```

`encoding = gamma22` heißt: Eingang linear → `x^(1/2,2)` → dann die
Stützstellen. `encoding = none` heißt: Stützstellen direkt auf linear.

## 11. Abnahme

| Prüfung | Bestehen | Scheitern |
|---|---|---|
| Negativ, Graukeil | log-log-Steigung 1,00 ± 0,02; Residuum ≤ 2 % | Kurve, Gamma oder WB im Negativ |
| Rückmosaik | Trägerenergie von `L̂` auf Grauflächen ≤ 1,0× Rauschen | sichtbares 2-px-Gitter |
| Farbfläche | Trägerenergie ≤ 1,5× lokales Rauschen | sichtbares 2-px-Gitter |
| Look, Graukeil | ≤ 3/255 gegen das JPEG der Referenz-Monochromkamera | an anderem JPEG gefittet |
| Gewichte | Patch-RMSE ≤ 5 % der linearen Helligkeit der Referenz-Monochromkamera | Matrix vor der Trennung |
| Stern | MTF50 achsparallel und 45° gemessen, kein Ring | Prozent ohne Messung |
| Zwei Kameras, Grau | dieselbe Kurve, ≤ 3/255 nach Mittelgrau-Anker | Look pro Gehäuse |
| Synthetisch | RMSE gegen `L_true` auf neutralen Szenen ≤ 1,5·σ; MTF50 achsparallel ≥ 0,35 c/px | — |

## 12. Performance-Budget

Referenz: AMD Ryzen 7 5700U, 16 Threads, f64 überall. 24 MP Negativ
(Einlesen bis TIFF) ≤ 3 s, 45 MP ≤ 6 s. Speicher ≤ 6 Vollebenen f64
(45 MP: ≤ 2,2 GB). Parallelreduktionen in fester Chunk-Reihenfolge
(deterministisch bitgenau zwischen Läufen).

**[Abweichung, gemessen]** seit 0.3.0: Der Default-Pfad (5.5 mit
Kohärenzring) braucht 3,3 s und zehn Vollebenen auf 24 MP (2,0 GB; 45 MP
hochgerechnet ≈ 6 s, 3,7 GB). Das Budget gilt weiter für `--mask off` und
`--mask adaptive` (2,2 s, sechs Ebenen). Der Mehraufwand ist der Preis
für +1,5 bis +3,8 dB in den bandbegrenzten Bedingungen (RESULTS „Coherence
ring“); er wird akzeptiert, bis eine Ebene gespart ist, ohne das Ergebnis
bitgenau zu ändern.

Umsetzung (**[gemessen]**, RESULTS „Phase 5“):

- Faltung: Jeder Ausgabewert summiert seine Taps in fester Reihenfolge
  `t = 0..n`; gerechnet werden 16 Ausgabewerte gleichzeitig in Registern
  (Lanes), die Spaltenfaltung in Bändern von 32 Zeilen × 512 Spalten
  (Quellzeilen bleiben im L2). Die Träger (±1) werden beim Füllen der
  Zeile bzw. als Zeilenvorzeichen in der Spaltenfaltung angebracht: zwei
  Zeilen- und drei Spaltenpässe für die drei Chrominanzschätzungen statt
  drei Modulationen und sechs Pässe — bitgleich zur Referenz
  (`lowpass_reference`, Test `fused_demodulation_matches_reference`).
- Speicher: Maskenwerte bleiben Blockraster und werden beim Mischen pro
  Pixel bilinear abgetastet; Kern-Zerlegung und Dubois-Kombination laufen
  in den Puffern der Schätzungen; die Mischung überschreibt `L̂`. Spitze
  im adaptiven Pfad: Mosaik, drei Schätzungen, `L̂`, Maskenebene = 6
  Vollebenen. Allokator mimalloc (glibc gibt 200-MB-Blöcke sofort an den
  Kernel zurück, jede neue Ebene zahlte ihre Seitenfehler neu).
- Determinismus: Parallelsummen (Grauwelt, Schleifenabbruch) werden pro
  Zeile gebildet und in Zeilenreihenfolge addiert; Test
  `separation_is_deterministic_across_thread_counts` (16 gegen 3 Threads,
  bitgleich).
- Defektprüfung: der Median der acht Nachbarn wird nur für Pixel außerhalb
  der Nachbarspanne gebildet (identisches Ergebnis, kein Sortieren pro
  Pixel).

## 13. Quellen

Alleysson, Süsstrunk, Hérault: Color demosaicing by estimating luminance and
opponent chromatic signals in the Fourier domain, CIC 2002; Linear
demosaicing inspired by the human visual system, IEEE TIP 2005.
Dubois: Frequency-domain methods for demosaicking of Bayer-sampled color
images, IEEE SPL 2005.
Liu, Yang, Chen: Frequency Enhancement for Image Demosaicking,
arXiv:2503.15800 (nur als Grenze, kein Netz in dieser App).
