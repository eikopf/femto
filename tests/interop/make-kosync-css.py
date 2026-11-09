"""Build the deterministic EPUB fixture for KOReader DOM style handling."""
from pathlib import Path
import zipfile

root = Path(__file__).resolve().parents[1] / "fixtures"
xml = '''<html xmlns="http://www.w3.org/1999/xhtml"><head><title>Sync styles</title>
<style>
.block { display: block; }
.inline { display: inline; }
.inherit { display: inherit; }
.preserve { white-space: pre; }
</style></head><body>
<h1>CSS positions</h1>
<p><span class="block">\n <b>Bold</b> tail block</span></p>
<div class="inline">\n <b>Bold</b> tail inline</div>
<p><span style="display:block">\n <b>Bold</b> tail inline style</span></p>
<div style="display:inline">\n <b>Bold</b> tail override</div>
<div class="inline"><span class="inherit">\n <b>Bold</b> tail inherited inline</span></div>
<div class="block"><span class="inherit">\n <b>Bold</b> tail inherited block</span></div>
<div class="preserve">\n <b>Bold</b> tail preserved</div>
<pre style="white-space:normal">\n <b>Bold</b> tail normal</pre>
</body></html>'''
with zipfile.ZipFile(root / "sample.epub") as source, zipfile.ZipFile(root / "kosync-css.epub", "w") as destination:
    first = next(name for name in source.namelist() if name.endswith(".xhtml") and "nav" not in name)
    for item in source.infolist():
        destination.writestr(item, xml if item.filename == first else source.read(item.filename))
