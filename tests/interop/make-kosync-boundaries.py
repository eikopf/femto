"""Build deterministic EPUB fixtures for KOReader tag and text chunk boundaries."""
from pathlib import Path
import zipfile

root = Path(__file__).resolve().parents[1] / "fixtures"
cases = {
    "kosync-case.epub": "<BODY><P>Upper <SPAN>inline</SPAN> tail</P><p>Lower paragraph</p><P>Third paragraph</P></BODY>",
    "kosync-entity-boundary.epub": "<body>" + "".join(
        "<p>" + "a" * 8190 + entity + "Z</p>" for entity in ["&amp;", "&#38;", "&#x26;"]
    ) + "</body>",
    "kosync-line-endings.epub": "<body><pre>a\r\nb\n\rc\rd\ne</pre></body>",
}
for name, body in cases.items():
    xml = '<html xmlns="http://www.w3.org/1999/xhtml"><head><title>Sync boundaries</title></head>' + body + '</html>'
    with zipfile.ZipFile(root / "sample.epub") as source, zipfile.ZipFile(root / name, "w") as destination:
        for item in source.infolist():
            destination.writestr(item, xml if item.filename == "EPUB/one.xhtml" else source.read(item.filename))
