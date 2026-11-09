"""Build an EPUB with repeated image elements and an image-only chapter."""
from pathlib import Path
import zipfile

root = Path(__file__).resolve().parents[1] / "fixtures"
chapters = {
    "EPUB/one.xhtml": '''<html xmlns="http://www.w3.org/1999/xhtml"><head><title>Images</title></head><body>
<h1>Repeated images</h1><p><img src="diagram.png" alt="First"/><img src="diagram.png" alt="Second"/></p>
<figure><img src="diagram.png" alt="Third"></img><figcaption>Same resource, different element</figcaption></figure>
</body></html>''',
    "EPUB/two.xhtml": '''<html xmlns="http://www.w3.org/1999/xhtml"><head><title>Image only</title></head><body><img src="diagram.png" alt="Image-only page"/></body></html>''',
}
with zipfile.ZipFile(root / "sample.epub") as source, zipfile.ZipFile(root / "kosync-images.epub", "w") as destination:
    for item in source.infolist():
        payload = chapters.get(item.filename, source.read(item.filename))
        if item.filename == "EPUB/book.opf":
            payload = payload.replace(b"</manifest>", b'<item id="diagram" href="diagram.png" media-type="image/png"/></manifest>')
        destination.writestr(item, payload)
    destination.writestr(zipfile.ZipInfo("EPUB/diagram.png", (1980, 1, 1, 0, 0, 0)), (root / "diagram.png").read_bytes())
