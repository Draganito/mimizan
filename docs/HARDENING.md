# Was die Härtung behoben hat

Stand dieses Zweigs: `cursor/release-hardening-f1b9`. Die
behobenen Punkte reichen bis Commit `aaea5bc`.
`main` ist unverändert. Die Programme nennen weiter die Version 0.20.15.

151 gemeldete Fehler sind behoben. Dieselbe Ursache haben oft mehrere
Prüfer gemeldet. Sie steht hier einmal, mit allen Nummern. Die Nummern
sind die des Prüfregisters.

Die Referenzbilder der Entwicklung und die 17 Fensterbilder sind Bit
für Bit gleich geblieben, ausser wo ein Abschnitt sagt, dass sich das
Ergebnis ändert. Die 24 Kodak-Bilder der Schlussmessung sind dieselben.

Am Ende steht, was in diesem Stand mit Absicht nicht geändert ist.

## Dateien

### Eine Datei wird ganz geschrieben, oder die alte bleibt

Nummern: G2-002, K1-001, K4-001, X4-001, X4-006, G2-005, G6-002,
K6-001, K3-007, L2-008, X1-014, K6-003, K6-012, K3-015, K1-014.

**Vorher.** Rezept, Export, JPEG, TIFF, DNG, Kameradatei und Look
wurden unter dem endgültigen Namen geöffnet. Damit war die alte Datei
sofort leer. Brach die Schreibe ab, lag eine halbe Datei unter dem
Namen, den man behalten wollte. Der letzte Puffer wurde beim Schliessen
verworfen, ein kurzer Schreibaufruf des TIFF-Schreibers galt als
Erfolg, und die Auflösungsangabe konnte einen Schreibfehler in einen
Absturz verwandeln. Die Maske wurde erst nach dem Negativ auf ihre
Grösse geprüft. Zwei Negative, deren Namen sich nur in Bytes ausserhalb
von UTF-8 unterscheiden, teilten sich eine Maske.

**Nachher.** Geschrieben wird in eine Geschwisterdatei, dann umbenannt.
Scheitert das, bleibt die alte Datei. Ein JPEG über 65535 Pixel wird
abgelehnt, bevor etwas geleert wird. Der letzte Puffer wird geprüft,
ein kurzer Schreibaufruf wird wiederholt, bis alle Bytes weg sind, die
Auflösung meldet ihren Fehler, und die Maske wird geprüft, bevor das
Negativ entsteht. Der Maskenname nimmt den Dateinamen als Bytes.
Commit `a15efd1`.

### Die Ausgabe überschreibt die Eingabe nicht

Nummern: L2-001, X4-002.

**Vorher.** `mimizan negative`, `dump` und `print` verglichen den
Ausgabepfad nicht mit der Eingabe. `-o` auf die eigene RAW-Datei
ersetzte das Negativ der Kamera. Das Fenster verweigerte genau das
schon.

**Nachher.** Dieselbe Prüfung wie im Fenster gilt für die Befehle, für
`-o`, für die Maske, für die Vorschau und für `--write-recipe`.
Commit `181aad9`.

### Ein Rezept, das sich nicht lesen liess, bleibt liegen

Nummern: K4-002, G2-006, X4-008.

**Vorher.** Ein Rezept mit einem Tippfehler, einem abgeschnittenen
Ende oder einem Schema einer neueren Fassung öffnete das Bild auf den
Vorgaben. Die Frage beim Gehen hiess wie immer «Rezept speichern?».
Speichern schrieb über die Datei, die nie gelesen worden war.

**Nachher.** Die Frage sagt, dass die Datei nicht gelesen wurde.
Speichern benennt sie zuerst in `.recipe.json.unread` um.
Commit `03e3f82`.

### Ein Schlüssel, den dieses Programm nicht kennt, wird abgelehnt

Nummer: K4-003.

**Vorher.** Ein unbekannter Schlüssel im Rezept wurde still
weggelassen. Ein Tippfehler und ein Schlüssel einer anderen Fassung
druckten ein anderes Bild, ohne ein Wort. Das Schema stand seit der
ersten Fassung auf 1.

**Nachher.** Rezept, Spitze, Entwicklung, Platte, Ausschnitt und
Split-Grade lehnen einen unbekannten Schlüssel mit seinem Namen ab.
Ein Rezept, das dieses Programm treu drucken kann, wird weiter gelesen.
Commit `18241df`. Das geschriebene Bild ändert sich, wenn ein Rezept
solche Schlüssel trug. Ein Rezept ohne sie bleibt gleich.

### Ein Rezept, das ein anderes Programm geändert hat, wird nicht ersetzt

Nummern: X4-009, X4-011.

**Vorher.** Zwei Fenster auf einem Ordner schrieben das Rezept
voreinander weg. Zwei Umwandlungen derselben Datei füllten dieselbe
`.part`. Änderungen am Bild, während eine andere Datei noch lud,
fielen ohne Frage weg. Ein Beenden in diesem Moment speicherte für die
andere Datei.

**Nachher.** Der Teilname trägt die Prozessnummer. Hat sich das Rezept
auf der Platte geändert, bleibt es, und die eigene Fassung heisst
`.recipe.json.other`. Solange eine andere Datei öffnet, ruhen die
Regler des Bildes auf der Bühne. Die Bytes entscheiden, auch wenn die
Uhr der Datei sich nicht bewegt hat.
Commits `8531ece`, `27205fc`, `cb8dd6a`, `2e8fb62`.

### Die Einstellungen werden einmal geschrieben und eine unlesbare Datei bleibt

Nummern: G1-002, G7-009, X4-015.

**Vorher.** `gui.json` wurde bei vielen Klicks sofort überschrieben,
und ein Pinsel, den die Ansicht zurückzog, schrieb sie Bild für Bild.
Ein Abbruch liess einen Stummel. Der wurde als «kein JSON» gelesen und
beim nächsten Mal durch die Vorgaben ersetzt: Ordner, Lesezeichen,
Pinsel und Schalter weg.

**Nachher.** Die Einstellungen werden markiert und einmal geschrieben,
in dem Bild, in dem kein Zeigerknopf mehr unten ist. Die Datei geht
über einen ganzen Schreibvorgang und einen neuen Namen. Eine Datei,
die kein JSON-Objekt ist oder sich nicht öffnen lässt, wird nicht
durch die Vorgaben ersetzt.
Commits `37944f1`, `8531ece`, `7711331`, `143b159`.

### Eine Kameradatei bleibt im Ordner `cameras`

Nummern: K4-004, X4-022.

**Vorher.** Hersteller und Modell wurden kleingeschrieben, Leerzeichen
wurden Unterstriche. Schrägstrich, Stern und `..` blieben. Eine Kodak
DCS Pro SLR/n landete in einem Unterordner, eine Pentax mit Stern im
Namen liess sich unter Windows nicht schreiben, und ein Modell mit
`../` zielte aus dem Ordner heraus.

**Nachher.** Was kein Buchstabe, keine Ziffer, kein Bindestrich, Punkt,
Unterstrich oder Klammernpaar ist, wird ein Unterstrich. Ein führender
Punkt auch. Commit `1793b11`.

### Eine fremde DNG neben einer X-Trans-Datei ist nicht die eigene

Nummern: X4-003, G6-005, X4-007.

**Vorher.** Jede nicht leere `<name>.dng` neben einer RAF galt als die
Umwandlung dieses Programms. Eine DNG eines anderen Programms ersetzte
die RAF in der Leiste, oder der Öffnen scheiterte und die RAF war nicht
mehr erreichbar. Die Umwandlung konnte eine Datei dieses Namens
ersetzen und löschte eine leere Datei dieses Namens.

**Nachher.** Die DNG gilt nur, wenn ihr Software-Kennzeichen mit
`xtrans2dng ` beginnt. Eine fremde Datei bleibt, die RAF bleibt in der
Leiste, und die Umwandlung ersetzt sie nicht.
Commits `8d37675`, `221222d`, `0621a8a`.

### Zwei Dateien eines Stammes teilen sich nicht das falsche Bild

Nummern: X4-004, X4-005, G2-001, G2-004, G6-003, G6-004.

**Vorher.** Das Rezept hiess nach dem Stamm, der Plattenexport schlug
genau den Namen vor, den eine zweite RAW im selben Ordner schon
benutzte. Zwei Bayer-Dateien eines Stammes teilten eine
Kleinste-Quadrate-DNG: die zweite öffnete das Bild der ersten. Das
Kästchen öffnete die schon offene Datei nicht neu. War es an, wurde
die DNG mit dem Rezept der RAW entwickelt, und die eigene Entwicklung
der DNG fiel weg. Liess sich die DNG nicht schreiben, ging eine ältere
Fassung auf, oder die Datei ging gar nicht auf, und das Kästchen zum
Abschalten lag hinter einem offenen Farbbild.

**Nachher.** Der Plattenexport heisst `<stamm>.linear.dng`. Nur eine
andere Bayer-Datei desselben Stammes schaltet die Kleinste Quadrate
aus. Eine Datei öffnet mit der Entwicklung der Datei, die wirklich
gelesen wird, und das Kästchen öffnet sie neu. Scheitert das
Schreiben, geht die gemessene Trennung auf, mit dem Grund im
Hinweis. Eine DNG einer älteren Fassung wird nicht still geöffnet.
Commits `d6a2601`, `221222d`, `524c5fe`, `9806d46`, `f4edd1d`.

## Das Bild

### Unter 1:1 ist die Platte dieselbe Farbe wie in der Datei

Nummer: G4-001.

**Vorher.** Bei voller Auflösung wird ein negativer Kanal auf null
gezogen und die anderen beiden geben denselben Anteil ihrer Distanz
zur Leuchtdichte ab. Auf einem gröberen Gitter fehlte dieser Schritt.
Das Fenster zeigte andere Farben als die 1:1-Ansicht, die eingepasste
Kopie und die Datei.

**Nachher.** Das grobe Gitter mischt über denselben Schritt.
Commit `941cf33`.

### Eine weiche Ellipse endet an ihrer Feder

Nummern: K2-001, K2-003, K2-004, K2-005.

**Vorher.** Der Abstand einer elliptischen Spitze war nur auf dem
Kreis und auf den Achsen richtig. Abseits davon reichte die Feder
weiter als angegeben, und ein gerader Strich durch die Mitte gab mehr
als die Blenden des Strichs. Eine Karte mit fast keinem Weg und eine
Feder, die zu schmal zum Abtasten ist, ergaben Gewicht 0, unendlich
oder keine Zahl. Eine sehr lange Spitze rechnete jeden Rasterpunkt
gegen alle Proben.

**Nachher.** Der Abstand ist der echte Abstand zur Ellipse. Kreise
bleiben, wo sie waren. Die Verweilzeit ist das Integral des Gewichts.
Eine Karte mit fast keinem Weg liegt, eine zu schmale Feder ist eine
harte Kante. Die Proben liegen im Kasten der Spitze, nicht im Kreis
einer zu grossen Reichweite. Ein Strich an der Rastergrenze wird so
gebaut, dass er noch wachsen kann.
Commits `dcfa19b`, `1935730`, `cb550b7`, `967a196`, `4d3fbff`,
`6ce3a37`. Weiche Ellipsen drucken anders als zuvor. Kreise nicht.

### Ein Strich aus der Datei ist derselbe wie der gemalte

Nummer: K2-002.

**Vorher.** Ein Strich, der im Fenster gemalt und dort nachgewachsen
war, druckte andere Gewichte als derselbe Strich, neu aus dem Rezept
gelesen. Der Export direkt nach dem Malen und der Export nach einem
Neustart waren nicht dieselbe Datei.

**Nachher.** Die Datei liest den Strich, der ganz gebaut wurde.
Commit `999a1cc` und `967a196`.

### Die Zählung einer grossen Abwedelung stimmt

Nummer: G5-001.

**Vorher.** Ab einer gewissen Fläche stand jeder soundsovielte Punkt
für ein Quadrat. Trafen zu viele Proben auf denselben Ton, blieb der
Rest im Histogramm auf dem unveränderten Ton. Mittelwert und
Schwarz- und Weissanteil wichen ab.

**Nachher.** Jeder erreichte Punkt wird mit seinem eigenen Ton
gezählt. Die Zeiten kommen vom Gitter. Commit `4e4bf88`.

### Die Schärfemessung sieht das ganze Bild

Nummern: K5-002, K5-003, G6-006, X1-009.

**Vorher.** Die Messung hielt drei volle Ebenen und las dann nur jeden
vierten Punkt. Von den Kanten, die durchkamen, blieben die ersten
2000 in Leserichtung. Ein grosses Bild wurde nur in seinem oberen
Teil gemessen. Eine Messung, die niemand mehr wollte, lief zu Ende
und hielt die Sitzung, auch nach «alles zurücksetzen» und nach «loslassen,
damit Platz wird».

**Nachher.** Der Gradient wird an dem Punkt gelesen, der ihn braucht.
Gibt es mehr gültige Kanten als behalten werden, werden sie über das
ganze Bild ausgedünnt. Kleinere Negative messen Bit für Bit wie zuvor.
Eine Messung, die nicht mehr gebraucht wird, endet. «Alles
zurücksetzen» behält eine Messung, die das Papier schon liest, das sie
hinterlässt.
Commits `57b6105`, `b90499c`, `17f8c1d`, `9fe5858`, `dc7facc`.
Bilder mit mehr als 2000 gültigen Kanten bekommen eine andere
gemessene Spitze.

### Die erste Farbtrennung schätzt ein Fenster nach dem anderen

Nummer: K5-001.

**Vorher.** Dubois, die ausgeschaltete Maske und die adaptive Maske
schätzten die Farbigkeit über das ganze Bild. Dubois hielt dabei bis
zu neun Ebenen doppelter Genauigkeit in der Grösse des Bildes. Eine
Datei von 24 Megapixeln brauchte dafür etwa 1,8 GB, bevor das Bild
gemischt war.

**Nachher.** Dubois und die ausgeschaltete Maske schätzen ein Fenster
von 2048 Punkten plus dem Rand des Verfahrens, eines nach dem anderen.
Jedes Filter dieser beiden Verfahren ist kürzer als der Rand, also
sind Farbigkeit und Leuchtdichte dieselben Bits wie über das ganze
Bild. Die Leuchtdichte wird einmal über das ganze Mosaik gerechnet.
Die adaptive Maske bleibt auf dem ganzen Bild: ein Fenster davon ist
nicht dasselbe. Die Speicherzahl für den üblichen Weg ist die grössere
von fünf vollen Ebenen und drei vollen Ebenen plus neun Ebenen eines
Fensters. Bei 24 Megapixeln sind das etwa 1,0 GB. Der zweite Durchgang
unter der Farbdecke rechnet weiter in seinen eigenen, kleineren
Fenstern. Die Zahl zählt dieses zweite Fenster nicht dazu.
Commit `0e729a2`. Die Referenzbilder sind dieselben.

### Schärfen läuft auf dem Bild, und eine USM, die nicht läuft, liest keine Maske

Nummern: K1-002, K1-003, K1-004, K2-006, K3-006.

**Vorher.** Das Bild wurde zuerst auf den Bogen gelegt. Schärfung und
Spitze schnitten es wieder heraus und legten es zurück. Eine USM
allein kopierte das ganze Bild, nur um es gleich zu ersetzen. Die
Maske wurde gelesen, gedreht und auf einen Bogen gelegt, auch wenn
bei Originalgrösse, bei einer Verkleinerung oder mit dem
Flächenkern keine USM laufen kann. Die Beschreibung sagte dann, die
USM sei maskiert gewesen. Drehen und Ausrichten kopierten die Ebene,
auch wenn nichts gedreht war.

**Nachher.** Geschärft wird das Bild, danach kommt der Bogen. Eine
USM allein schärft die Ebene, die sie bekommt. Die Maske wird nur
gelesen, wenn eine USM läuft. Eine lineare Ausgabe leiht ein Bild,
das sie nicht dreht. Die Beschreibung nennt eine Maske nur, wenn die
USM gelaufen ist.
Commits `8b1c10b`, `67d7605`, `8eb06f0`, `6ff96a2`, `7081e3f`.
Die Pixel bleiben. Der Satz in der Beschreibung ändert sich, wo
keine USM lief.

### Die Speicherzahl nennt, was die Ausgabe wirklich hält

Nummern: G2-010, K2-008.

**Vorher.** Der Grauexport einer Farbdatei zählte die gemischte Ebene
nicht, die er erst baut. Die Raster der Striche hatten keine eigene
Grenze in der Schätzung, an der Befehl und Fenster eine Ausgabe
ablehnen.

**Nachher.** Der Grauexport einer Farbdatei zählt die gemischte Ebene.
Ein Sensor, der schon grau ist, und eine Platte zählen sie nicht
dazu. Die Schätzung zählt die Bytes, die die Striche halten. Ein
Rezept ohne Striche bleibt bei derselben Zahl.
Commits `68df4a7`, `93ee073`. Die Zahl im Fenster für diesen Export
ist eine andere.

### Eine Farbdatei hält die volle Mischung nicht mehr neben der Trennung

Nummer: G4-002.

**Vorher.** Eine Farbsitzung behielt eine volle Mischung in doppelter
Genauigkeit, die niemand las. Jede Ansicht und jeder Export mischten
neu.

**Nachher.** Die Sitzung lässt diese Ebene los, sobald Breite und
Höhe bekannt sind. Ein grauer Sensor behält seine Ebene: dort ist sie
das Bild. Commit `2ce96c6`.

### Das Bild kommt, bevor die Histogramme gezählt sind

Nummer: G5-002.

**Vorher.** Jeder Schritt an Mischung, Geraderichten oder Ausschnitt
zählte zuerst das Histogramm der vollen Auflösung. Das kleine Bild
wartete darauf.

**Nachher.** Das Bild wird geschickt, die Histogramme folgen.
Commit `31b69b4`.

### Die Vorschau lässt eine losgelassene Sitzung los

Nummern: G5-003, G6-009.

**Vorher.** «Loslassen, damit Platz wird» liess die Ebenen der Sitzung
frei. Der Vorschaupfad hielt die Codes, die Zählung und die kleinen
Ebenen der vorigen Datei weiter.

**Nachher.** Die Vorschau lässt das los, sobald ihr Bild geschickt
ist. Eine Vorschau, die noch nicht angefangen hat, wird nicht
angefangen. Commits `a40ec67`, `3932a09`.

### Kleinste Quadrate: nur ein Bayer-Muster, und die Ebenen, die niemand liest, gehen

Nummern: L1-001, L1-002, L1-003.

**Vorher.** Ein 2×2-Mosaik mit Rot, das kein Bayer ist, wurde als
Bayer entwickelt. Smaragd wurde als Grün genommen. Die laufenden
Summen der Bank liefen auf einem Kern über das ganze Bild, einmal je
Vorwissen. Ebenen, die nach dem ersten Schritt niemand las, blieben
bis zum Ende.

**Nachher.** Es gelten die vier Namen, die der Kern liest. Jedes
Fenster schreibt seine eigenen Summen. Eine Ebene wird fallengelassen,
sobald nichts sie liest. Die Zf-DNG dieser Rechnung hat denselben
Hash. Commits `695842a`, `96573a4`, `6d6ae7a`.

### `bench bayer` misst die Bedingung, die man ihm gibt

Nummern: L2-002, X3-001.

**Vorher.** Lag von einem früheren Lauf eine DNG oder ein Bild eines
Wandlers im Ordner, wurde es wiederverwendet und gegen die neue
Wahrheit gewertet. Ein Lauf mit Unschärfe oder doppeltem Massstab
nach einem scharfen Lauf nannte Zahlen, die nicht zu dieser Bedingung
gehörten.

**Nachher.** Jeder Lauf schreibt das Mosaik und lässt jeden Wandler
laufen. Commit `11e1117`.

## Lesen, ohne das Programm zu beenden

### Eine RAW liegt im Speicher, und eine Panik des Decoders ist ein Fehler

Nummern: K3-001, K3-002, X2b-002, X4-012.

**Vorher.** Die Datei wurde in den Adressraum gelegt. Zog man die
Karte oder brach die Freigabe ab, endete das Programm mit einem
Signal, das kein Fangen sieht. Der Decoder wurde ausserhalb seiner
eigenen Sicherung gerufen. Eine Datei, auf der er abbricht, beendete
den Befehl mit einer Panik. Das Fenster fing das in seinen beiden
Arbeitern, der Befehl nicht.

**Nachher.** Die Datei wird gelesen und als Puffer übergeben. Ein
Abbruch des Decoders ist ein Lesefehler. Commit `abb685f`. Die
X-Trans-Umwandlung in der festgehaltenen Kiste `mimizan-lab-xtrans`
legt die Datei weiter in den Adressraum. Das ist eine Folge in jenem
Projekt, nicht in diesem Zweig.

### Eine Fuji Super-CCD und eine Gleitkomma-DNG ohne Weisspunkt

Nummern: K3-003, K3-004.

**Vorher.** Eine Fuji mit den Bildpunkten auf dem Diagonalraster wurde
als aufrechtes Bayer-Bild angenommen und um 45 Grad gedreht
entwickelt. Eine Gleitkomma-DNG ohne Weisspunkt wurde durch
2^Bits − 1 geteilt. Die Werte liegen um 0 bis 1, das Bild wurde
schwarz.

**Nachher.** Super-CCD wird mit einem Satz abgelehnt. Fehlt der
Weisspunkt bei Gleitkomma, gilt 1,0. Commit `abb685f`.

### Die Grösse wird geprüft, bevor der Speicher verlangt wird

Nummer: K3-005.

**Vorher.** Breite und Höhe aus dem Kopf bestimmten die Reservierung,
bevor irgendwer sie prüfte. Eine Angabe, die nicht in den Rechner
passt, brach die Reservierung ab. Das ist kein Fehler, den man fängt.

**Nachher.** Eine Angabe über der Grenze wird abgelehnt, bevor die
Punkte gelesen werden. Commit `953c697`.

### Die Höhe einer Platte wird mit einer Prüfung verdreifacht

Nummern: X2b-001, K6-011.

**Vorher.** Die Rahmenhöhe steht in der Beschreibung der Datei. Das
Dreifache konnte überlaufen. Ein Wert, der danach wieder zur Höhe der
Ebene passte, kam durch, und die Reservierung der Ebene endete das
Programm.

**Nachher.** Höhe 0 und ein Überlauf sind der Fehler «die Platte ist
so und so viele Zeilen hoch». Commit `8851add`.

### Eine Beschreibung, die da ist und kein Text ist, ist ein Fehler

Nummer: K6-004.

**Vorher.** Jeder Fehler beim Lesen der Beschreibung galt als «keine
Beschreibung». Eine Platte wurde dann als ein graues Bild von drei
Rahmen Höhe gedruckt, und ein Bereich über 1 als 1.

**Nachher.** Eine fehlende Beschreibung bleibt eine fehlende. Eine
Beschreibung, die sich nicht lesen lässt, ist ein Fehler des Lesens.
Commit `753fc5b`.

### Getrennte Farbebenen sind kein Graubild

Nummer: K6-005.

**Vorher.** Ein TIFF, das die Farben in getrennten Ebenen hält, meldet
eine Probe je Punkt. Gelesen wurde die erste Ebene, ohne Fehler, und
als Negativ gedruckt.

**Nachher.** Grau wird nur als Grau mit 8 oder 16 Bit gelesen, eine
Maske nur als Grau mit 8 Bit, und das, bevor die Punkte gelesen
werden. Commit `19ac0b2`.

### Ein Rechteck ohne Punkt ist nicht Schwarz

Nummer: K6-007.

**Vorher.** Ein Messrechteck ausserhalb des Bildes gab Mittel 0 und
Streuung 0. Die Keil-Kalibrierung nahm das als gemessenes Schwarz.

**Nachher.** Kein Punkt ist keine Zahl. Die Kalibrierung lehnt ein
leeres Rechteck ab und eines, das über das Bild hinausreicht.
Commit `4c4e924`.

### Ein TIFF liegt auf geraden Offsets

Nummer: K6-002.

**Vorher.** Etwa die Hälfte der TIFFs dieses Programms legte Werte,
Streifen und das Verzeichnis auf ungerade Offsets. Die Fassung 6 des
Formats verlangt eine Wortgrenze. Der DNG-Schreiber derselben
Bibliothek füllt auf, der TIFF-Schreiber tat es nicht.

**Nachher.** Die Beschreibung wird gespeichert, wie sie geschrieben
wurde. Ein Nullbyte nach einem ungeraden Wert ausserhalb des
Verzeichnisses und nach einem ungeraden Streifen hält den nächsten
Offset gerade. Ein auffüllendes Leerzeichen in der Beschreibung hätte
zwanzig Referenzdateien geändert. Das unterblieb.
Commits `5c22a82`, `20ad568`. Die Bildpunkte bleiben.

### Eine Kameradatei, die sich nicht laden lässt, stoppt den Druck

Nummer: L2-024.

**Vorher.** Jeder Fehler auf dem Weg zur Vorgabe der USM aus der
Kameradatei wurde verschluckt. Eine Datei, die da ist und sich nicht
laden lässt, druckte ohne die Schärfung der Kamera und ohne ein Wort.
`negative` behandelte dieselbe Datei als Fehler.

**Nachher.** Eine Kameradatei, die sich nicht laden lässt, stoppt
`print` und nennt den Pfad. Eine Datei, die kein TIFF ist, wird
weiter abgelehnt, wenn der Druck sie liest, mit dem Satz, den die
Referenz erwartet. Keine Kamera und keine Datei bleiben «keine
Vorgabe». Commits `56d92d5`, `61da17b`.

## Das Fenster

### Ein zu grosses Bild beendet das Programm nicht

Nummern: C1-001, X1-001, X1-002.

**Vorher.** Lupe und scharfes Fenster luden das Bild als eine Textur
in voller Grösse. Die Windows-Fassung verlangt höchstens 8192 Pixel
auf einer Seite. Darüber verschwand das Fenster. Die Dateien lagen
dann schon, der nächste Start las sie und endete wieder.

**Nachher.** Die Lupe schneidet ein Bild, das grösser ist als die
grösste Textur der Karte, in Kacheln. Das scharfe Fenster wird nie
eine Textur, die die Karte nicht nimmt. Commits `9552d85`, `f686320`.

### Eine Stufe ohne Platz macht den Zoom nicht zu einer Nichtzahl

Nummern: X2a-001, G3-002.

**Vorher.** Liessen die Fenster der Bühne keinen Platz, wurde die
Einpassung 0. Eine Vierteldrehung rechnete 0 geteilt durch 0. Der
Zoom blieb eine Nichtzahl, die Bühne leer, die Beschriftung «NaN %».
Mit dem Ausschnitt an führte das jedes Bild zum Abbruch.

**Nachher.** Eine Stufe ohne Platz lässt den Zoom in Ruhe.
Commit `d9d329b`.

### Ein Filter, durch den nichts durchkommt, ist abgeblendet

Nummer: X2a-002.

**Vorher.** Eine Kameradatei, deren ganzes Gewicht auf Blau liegt,
machte das Feld des Rotfilters zu einer Nichtzahl. Ein Klick darauf
beendete das Fenster.

**Nachher.** Ein Filter, den die Antwort des Sensors nicht durchlässt,
ist abgeblendet und lässt sich nicht wählen. Commit `950fe0e`.

### Eine Datei, die während einer Frage fällt, wird nicht geöffnet

Nummern: G2-003, G6-001, X1-006.

**Vorher.** Tasten waren gesperrt, solange die Speicherfrage oder
«Ersetzen?» offen war. Eine fallengelassene Datei nicht. Sie öffnete
hinter der Frage, und die Antwort schrieb das neue Bild unter den
Namen der vorigen Datei.

**Nachher.** Eine Datei, die fällt, während eine Frage oder der
Speicherdialog offen ist, wird nicht geöffnet. Commit `e8d7e12`.

### Der Ordner wird ausserhalb des Bildes gelesen

Nummern: G1-001, G2-007, G3-001, G6-007, G7-002, G7-003, G7-005,
X1-003, X1-004, X4-010.

**Vorher.** Ordnerliste, eine Statusabfrage je Eintrag und die Paarung
von RAW und DNG liefen in dem Bild, das das Fenster zeichnet. Auf
einer langsamen Freigabe stand das Fenster. Mit dem Kästchen aus
wurde das Verzeichnis einmal je DNG neu gelesen. Die Leiste legte
jedes Bild des Ordners an, auch ausserhalb des Sichtbaren. Eine
Rezeptlesung von wenigen Kilobytes nach dem Öffnen und `gui.json` im
eigenen Einstellungsordner bleiben auf diesem Pfad.

**Nachher.** Der Ordner wird auf einem eigenen Faden gelesen. Die
Leiste behält ihre Höhe, solange gelesen wird. RAW und DNG werden in
einem Durchgang gepaart. Gezeichnet werden die Rahmen, die auf dem
Schirm sind. Ein Name wird zuerst angesehen. Eine Verknüpfung wird
verfolgt, alles andere nimmt den Typ aus der Liste.
Commits `ba9eff4`, `16cdeeb`, `869c045`, `3910ddc`, `9d42f5a`,
`b5cced1`.

### Ein Ordner, der sich nicht lesen lässt, lässt den vorigen stehen

Nummern: G7-006, G7-010.

**Vorher.** Ein unlesbarer Ordner leerte die Leiste und schrieb den
gemerkten Ordner mit leer. Ein neuer Ordner ohne Bild schickte keine
Liste. Die Miniaturen des vorigen Ordners liefen weiter.

**Nachher.** Ein unlesbarer Ordner bleibt der, der auf dem Schirm ist,
und wird nicht gespeichert. Ein leerer Ordner schickt seine Liste
einmal, auch leer, und die Miniaturen des vorigen Ordners hören auf.
Commits `aa4434c`, `53ac6a4`, `ce54027`.

### Eine Datei, die in die Leiste zurückkommt, wird wieder bebildert

Nummern: G7-001, G7-004.

**Vorher.** Kam eine Datei innerhalb derselben Ordnerlesung in die
Leiste zurück, kannte der Arbeiter den Auftrag und warf ihn weg. Die
Kachel blieb auf «…». Die Kachel einer Kleinste-Quadrate-DNG stand
nicht aufrecht.

**Nachher.** Eine zurückgelegte Datei wird wieder bebildert. Die
Kachel steht aufrecht. Commits `b3734f5`, `234e980`.

## Die Lupe

### Die Lupe findet den Befehl in der Mac-App und die Kameradateien, die mitkamen

Nummern: C1-002, D1-001, C1-013, D1-009, X3-002.

**Vorher.** Im Mac-Zip lag der Befehl in `Mimizan Lab.app`, die Lupe
suchte neben sich, unter `target` und auf dem Pfad. Jede Bayer-Datei
scheiterte. Die Kameradateien suchte sie zuerst im Arbeitsordner und
in fünf Ordnern darüber. Ein Ordner namens `cameras` dort ersetzte
die des Programms, und dasselbe RAW wurde ein anderes Bild.

**Nachher.** Sie findet den Befehl im App-Bündel und die Kameradateien
neben dem Programm, nach der Regel des Kerns.
Commit `5cae416`.

### Eine Entwicklung in der Lupe endet, und sie lässt sich anhalten

Nummern: C1-003, C1-004, C1-006, C1-007, C1-008, C1-009, C1-010,
C1-015, C1-016, C1-017, C1-028, D1-002, X1-005, X1-007, X1-008,
X1-015, X1-016, X2a-003, X2a-004.

**Vorher.** Eine Panik im Arbeitsfaden liess das Fenster auf
«Developing…». Der Schwanz einer Fehlermeldung wurde an einem Byte
abgeschnitten. Lag das Byte in einem Zeichen aus mehreren Bytes,
endete der Faden, und das Fenster hing. Das Fenster zeichnete sich
ohne Pause neu, solange es offen war. Volle Bilder wurden auf dem
Faden der Oberfläche umgerechnet, und eine Kopie jedes Bildes blieb
im Speicher, obwohl nur Breite und Höhe gelesen wurden. Schliessen
während einer Arbeit liess die fremden Programme laufen und ihre
grossen Zwischenbilder liegen. Unter Windows öffnete jedes von ihnen
ein Konsolfenster. Der Auswahldialog startete je gewählter Datei
einen Faden, der die ganze RAW las. Eine Fehlermeldung nannte die
Ursache nicht. Das Textfeld war die Datei: die linke Seite konnte aus
einer anderen Datei kommen als die rechte. `view.txt` behauptete mehr,
als geschrieben war. Ein unterbrochener Lauf galt beim nächsten Start
als fertig. Ein Pfad, der kein UTF-8 ist oder mit einem Leerzeichen
beginnt oder endet, liess sich nicht öffnen.

**Nachher.** Eine Entwicklung endet, was auch geschieht, und sie lässt
sich anhalten. Der Schnitt liegt auf einer Zeichengrenze. Der Titel
wird nur geschickt, wenn er sich ändert, und die Arbeiter wecken das
Fenster, wenn sie etwas haben. Das Bild wird im Arbeiter gebaut. Die
Zwischenbilder werden entfernt, die Kinder beim Schliessen beendet.
Unter Windows ohne Konsolfenster. Eine Vorschau, ein Faden. Der
Fehler nennt seine Ursache. Gemerkt wird die Datei, die die Seiten
zeigen. Ein Lauf, der nicht nur liest, nimmt die Markierung weg,
bevor er schreibt. Ein Pfad bleibt ein Pfad.
Commits `6e05318`, `ce9798c`.

### Die Arbeitsmappe gehört dieser Datei

Nummern: C1-005, X4-014.

**Vorher.** Die Mappe hiess nach dem Stamm. `photo.NEF` und
`photo.DNG` zeigten dieselben Bilder. Eine Datei, deren Name kein
UTF-8 ist, teilte `picture.mimizan-compare` mit jeder anderen solchen
Datei. `view.txt` merkte sich nur den Namen der Ansicht.

**Nachher.** Die Mappe heisst nach dem ganzen Dateinamen. Länge und
Änderungszeit der RAW stehen neben dem Ansichtsnamen. Eine RAW, deren
Name kein Text ist, bekommt ihre eigene Mappe. Commit `96dabe2`.

### Die Lupe liest ein TIFF aus ihrer Mappe mit der Grenze des Kerns

Nummer: C1-018.

**Vorher.** `negative.tif` und die Dateien der Werkzeuge wurden ohne
Grösse gelesen. Die Rahmenhöhe der Platte wurde ungeprüft
verdreifacht.

**Nachher.** Dieselbe Grenze wie der Kern. Eine Platte mit der falschen
Höhe und ein TIFF mit den falschen Proben werden im Test abgelehnt.
Commits `d67dbc1`, `92a0e8d`.

### 1:1 ist ein Bildpunkt auf einem Schirmpunkt

Nummer: C1-020.

**Vorher.** 1:1 setzte einen egui-Punkt je Bildpunkt. Auf einem Schirm
mit 125 oder 150 Prozent wurden manche Zeilen verdoppelt und manche
nicht.

**Nachher.** 1:1 teilt durch die Punkte je Schirmpunkt, wie das
Fenster. Die Beschriftung zeigt den Schirmanteil.
Commit `9ea6b65`.

### Die Werkzeugwahl liegt bei den Einstellungen

Nummern: C1-014, D1-009.

**Vorher.** Die zwei Programmpfade der Lupe lagen neben der
Programmdatei. Unter `/usr/bin` und unter `Program Files` darf ein
Benutzer dort nicht schreiben. Die Wahl war bei jedem Start weg.

**Nachher.** Sie liegt im Einstellungsordner der Dunkelkammer. Den
Ordner der Einstellungen und den des Zwischenspeichers nennt eine
Stelle, der Kern. Commits `9ef3c71`, `87f6f4b`.

### Ein Hochformat wird auf dieselbe Fläche beschnitten

Nummer: C1-027.

**Vorher.** Der Ausschnitt lag in der Orientierung des Sensors. Die
Dateien der anderen Verfahren kommen schon gedreht zurück. Ein
Hochformat fiel durch beide Prüfungen und blieb ganz, wo dasselbe
Querformat beschnitten wurde.

**Nachher.** Ein Hochformat der anderen Verfahren wird auf dieselbe
Fläche beschnitten wie die eigene Seite. Commit `2a57723`.

### Die Vorschau steht richtig herum

Nummer: C1-032.

**Vorher.** Eine Vorschau, die auf dem Kopf lag, blieb so. Gewendet
wurde nur bei den Orientierungen 5 bis 8 und nur, wenn das Bild
breiter als hoch war.

**Nachher.** Eine auf dem Kopf gespeicherte Vorschau wird richtig
herum gezeigt. Ein JPEG mit einem zweiten Scan endet weiter am
ersten Start des Bildes. Commit `e4212a5`.

### Die Statuszeile, das Textfeld, die Reste

Nummern: C1-025, C1-026, C1-029.

**Vorher.** Ziffern und Pfeile im Pfadfeld schalteten das andere
Verfahren um. Nach den Kleinste-Quadraten oder nach X-Trans allein
blieb der letzte Fortsatz stehen, als liefe die Arbeit weiter. Ein
Ordner hiess «No such file». Profile von RawTherapee und die Maske
des Negativs blieben in der Mappe, und nichts las sie wieder.

**Nachher.** Eine Ziffer oder ein Pfeil im Pfadfeld schaltet nichts
um. Die Zeile sagt bereit, wenn eine Arbeit sauber endet. Profil und
Maske werden entfernt, sobald das Werkzeug zurück ist.
Commits `00f4924`, `96b1ad1`, `99449f4`.

### Die Lupe benutzt die sRGB-Kurve des Kerns, und die Aufrufe haben Namen

Nummern: C1-019, C1-023, C1-024, C1-030, C1-031, X3-006.

**Vorher.** Die Lupe hielt eigene Abschriften der sRGB-Kurve, der
Dateiendungen, der Kamerazeilen und des Plattenlesens. Sie wichen an
einigen Stellen ab. `ui` tat vier Dinge. Entwicklung und die linke
Seite waren eine Funktion zweimal, mit nackten Wahrheitswerten an den
Aufrufen. Die Beschreibung der Kiste nannte etwas anderes, als das
Programm ist. Die riskanten Stellen hatten keinen Test.

**Nachher.** Die sRGB-Kurve ist die des Kerns. Die anderen
Abschriften bleiben, wo sie sich mit Absicht unterscheiden oder wo
der Kern keine Funktion dafür hat. Das Bild ist die Dialoge, die
Tasten, die Leiste und die Bilder. Entwicklung, die Seiten und die
Proben eines anderen Verfahrens sind am Aufruf benannt. Die
Beschreibung sagt, was das Programm ist. Eine Platte mit der falschen
Höhe und ein TIFF mit den falschen Proben haben einen Test.
Commits `a368877`, `a92ed7b`, `0fb4948`, `79cdf0e`, `92a0e8d`.

### macOS

Nummern: C1-022, C1-034, C1-035.

**Vorher.** Die Mac-Kiste hatte ausserhalb von macOS einen Stub ohne
Aufrufer. Die Dunkelkammer band sie nur auf dem Mac. Die Ordnerfrage
sagte «negatives». Ein Kauf sagte «Unlocked. Thank you.», ohne zu
schauen, ob der Laden den Kauf schon führt. `build.rs` versteckte,
warum `xcrun` scheiterte.

**Nachher.** Ausserhalb von macOS ist die Kiste leer, der Stellvertreter
ist weg. Die Ordnerfrage bittet um RAW-Dateien. Ein Kauf sagt
freigeschaltet erst, wenn der Laden ihn führt. Die Hinweise in
`apple.rs`, die einen Mac-Lauf brauchen, sind nicht auf einem Mac
gelaufen. Die CI übersetzt sie.
Commits `18e1098`, `19ff0a7`, `310e0ef`. Die beiden Systemfragen
bleiben modal. Das ist die bewusste Entscheidung C1-012, kein
offener Fehler.

### Der gemerkte Ordner hängt kein Volume ein

Nummer: C1-011.

**Vorher.** Das Lesezeichen des Ordners wurde beim Start auf dem
Hauptfaden aufgelöst, nur mit dem Merkmal für die Sandbox. Ein Volume,
das nicht da war, konnte eingehängt werden, und das Fenster wartete.

**Nachher.** Die Auflösung läuft auf einem eigenen Faden, ohne
Oberfläche und ohne Einhängen. Ein Ordner, der nicht da ist, bleibt
zu. Das Fenster ist vorher da. Hat man schon einen Ordner geöffnet,
ersetzt das späte Ergebnis ihn nicht. Auf einem Mac ist das nicht
gelaufen. Commit `57ad2ac`.

### Das offene Bild behält seinen Ordner

Nummer: C1-021.

**Vorher.** Die Freigabe eines Ordners fiel, sobald ein anderer
Ordner gewählt wurde, auch wenn das offene Bild noch im alten Ordner
lag. Das Rezept daneben wurde abgelehnt.

**Nachher.** Die alte Freigabe bleibt, bis dieses Bild zu ist. Ein
Schreiben neben die Datei ist in beiden Ordnern erlaubt. Die Leiste
zeigt den neuen Ordner. Auf einem Mac ist das nicht gelaufen.
Commit `548cbb1`.

### Die Speicheradresse bleibt bis zum Schreiben

Nummer: C1-033.

**Vorher.** Die Adresse des Speicherdialogs blieb für die ganze
Laufzeit, wenn der Zugriff anfing, und wurde sofort losgelassen,
wenn er nicht anfing. In beiden Fällen konnte das Schreiben die
Datei verlieren, die der Dialog freigegeben hatte.

**Nachher.** Die Adresse wird gehalten, bis der Export geschrieben,
abgelehnt oder abgebrochen ist, und der Zugriff endet dann. Auf
einem Mac ist das nicht gelaufen. Commit `aaea5bc`.

## Veröffentlichung

### Nur der Veröffentlichungsschritt darf schreiben

Nummer: D1-004.

**Vorher.** Der ganze Veröffentlichungsablauf hatte ein Schreibrecht
auf das Depot. Das Recht blieb in der ausgecheckten Kopie, während
fremde Aktionen und `cargo install` liefen. Die Signierung liess
Schlüssel auf der Platte. Jede Aktion hing an einem verschiebbaren
Namen. `cargo-deb` kam ohne festgehaltene Fassung.

**Nachher.** Nur der Schritt, der veröffentlicht, darf schreiben. Jede
Kopie wirft das Recht weg. Die Aktionen sind auf einen Commit
festgehalten. Eine Signierdatei wird entfernt, wenn ihr Schritt endet,
und ein fehlgeschlagener Schritt bleibt fehlgeschlagen. `cargo-deb`
ist die Fassung 3.8.0, festgehalten.
Commits `5ffc695`, `7558815`.

### Ein fehlgeschlagener Windows-Check stoppt den CI-Schritt

Nummer: D1-006.

**Vorher.** Drei Prüfungen liefen nacheinander. Nur die letzte
entschied. Eine Konsolmarkierung an `mimizan-gui.exe` oder ein
fehlender Import in `mimizan.exe` liess den Schritt grün. Ein
fehlgeschlagenes `cargo build` auch.

**Nachher.** Ein fehlgeschlagenes Bauen oder eine fehlgeschlagene
Prüfung beendet den Schritt. Die drei Prüfungen laufen in dem
Schritt. Commit `f4cce42`.

### Die Store-Prüfung läuft nur in der CI und stellt die Maschine zurück

Nummer: D1-011.

**Vorher.** `make_msix.ps1` entfernte die installierte Store-Fassung
von Mimizan Lab, schaltete das Sideload an der Maschine an und schrieb
den Wert nicht zurück. `make_release.ps1` rief das nach dem Zip immer
auf. Auf der Maschine des Autors war die Store-Fassung danach weg,
und das Sideload blieb an.

**Nachher.** Die Prüfung, die installiert, läuft nur, wenn `CI`
gesetzt ist, und sie schreibt die Richtlinie zurück. Ausserhalb der
CI bleibt die installierte Fassung. Das Paket wird weiter gepackt und
signiert. Commit `8b48e62`.

## Nicht in diesem Stand

**C1-012** bleibt, mit Absicht. Die beiden Systemfragen auf dem Mac
laufen modal, bis sie sich schliessen. Eine Scheibe würde die
Aufrufer ändern. Commit `2cdb7c9` sagt das im Kommentar.

Was im Register noch offen ist, ist in diesem Dokument nicht als
behoben geführt.
