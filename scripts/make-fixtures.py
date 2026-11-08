"""Generate small original books for document and layout regression tests."""

from pathlib import Path
from io import BytesIO
import struct
import zlib
from zipfile import ZipFile, ZipInfo, ZIP_DEFLATED, ZIP_STORED
from PIL import Image

root = Path(__file__).resolve().parent.parent / "tests" / "fixtures"
root.mkdir(parents=True, exist_ok=True)
paragraph = "A quiet page keeps its place when the font changes. Words flow across lines without losing their source positions. "
body = """<h1 id="start">Femto reading sample</h1>
<p>This is an original test book with <strong>bold text</strong>, <em>italic text</em>, and ordinary punctuation.</p>
<p>Mixed directions: English שלום עולם English. Combining marks: café, naïve. Ligatures: office, affinity.</p>
<p>Long tokens: abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789.</p>
""" + "\n".join(f"<p>{paragraph * 3}</p>" for _ in range(14))
files = {
    "META-INF/container.xml": '<?xml version="1.0"?><container xmlns="urn:oasis:names:tc:opendocument:xmlns:container" version="1.0"><rootfiles><rootfile full-path="EPUB/book.opf" media-type="application/oebps-package+xml"/></rootfiles></container>',
    "EPUB/book.opf": '''<?xml version="1.0"?><package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="id"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:identifier id="id">urn:femto:fixture:one</dc:identifier><dc:title>Femto reading sample</dc:title><dc:creator>Femto contributors</dc:creator><dc:language>en</dc:language></metadata><manifest><item id="one" href="one.xhtml" media-type="application/xhtml+xml"/><item id="two" href="two.xhtml" media-type="application/xhtml+xml"/><item id="nav" href="nav.xhtml" properties="nav" media-type="application/xhtml+xml"/><item id="css" href="style.css" media-type="text/css"/></manifest><spine><itemref idref="one"/><itemref idref="two"/></spine></package>''',
    "EPUB/nav.xhtml": '<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body><nav epub:type="toc"><ol><li><a href="one.xhtml#start">Opening</a></li><li><a href="two.xhtml">Another chapter</a></li></ol></nav></body></html>',
    "EPUB/style.css": "body { font-size: 16px; } h1 { font-size: 24px; font-weight: bold; } strong { font-weight: bold; } em { font-style: italic; }",
    "EPUB/one.xhtml": '<html xmlns="http://www.w3.org/1999/xhtml"><head><title>Opening</title><link rel="stylesheet" href="style.css"/></head><body>' + body + '</body></html>',
    "EPUB/two.xhtml": '<html xmlns="http://www.w3.org/1999/xhtml"><head><title>Another chapter</title></head><body><h1>Another chapter</h1><p>Chapter navigation resumes at a stable source location.</p></body></html>',
}
with ZipFile(root / "sample.epub", "w") as archive:
    archive.writestr(ZipInfo("mimetype"), "application/epub+zip", compress_type=ZIP_STORED)
    for path, text in files.items():
        archive.writestr(ZipInfo(path), text, compress_type=ZIP_DEFLATED)
(root / "sample.txt").write_text("Femto plain text\n\n" + "\n\n".join([paragraph * 4] * 12), encoding="utf-8")


def chunk(kind, payload):
    return struct.pack(">I", len(payload)) + kind + payload + struct.pack(">I", zlib.crc32(kind + payload))


def png(width, height, depth, color, raw, *, interlace=0, before=()):
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, depth, color, 0, 0, interlace)) + b"".join(chunk(k, v) for k, v in before) + chunk(b"IDAT", zlib.compress(raw)) + chunk(b"IEND", b"")


# Original synthetic raster fixtures exercise alpha, palette, bit depth, and Adam7.
rgba = Image.new("RGBA", (600, 400))
rgba.putdata([(x * 255 // 599, y * 255 // 399, 96, 128 if x < 150 else 255) for y in range(400) for x in range(600)])
image_bytes = BytesIO()
rgba.save(image_bytes, format="PNG")
(root / "diagram.png").write_bytes(image_bytes.getvalue())
rgb = Image.new("RGB", (320, 240))
rgb.putdata([(x * 255 // 319, y * 255 // 239, (x + y) * 255 // 558) for y in range(240) for x in range(320)])
rgb.save(root / "gradient.jpg", quality=92, subsampling=0)
rgb.save(root / "progressive.jpg", quality=92, subsampling=0, progressive=True)
tall = Image.new("L", (120, 1000))
tall.putdata([32 if (x // 10 + y // 20) % 2 else 224 for y in range(1000) for x in range(120)])
tall.save(root / "tall.png")
(root / "alpha.png").write_bytes(png(3, 1, 8, 6, b"\0" + bytes([0, 0, 0, 0, 0, 0, 0, 128, 255, 0, 0, 255])))
(root / "gray-alpha.png").write_bytes(png(3, 1, 8, 4, b"\0" + bytes([0, 0, 0, 128, 255, 255])))
(root / "palette.png").write_bytes(png(3, 1, 8, 3, b"\0\0\1\2", before=[(b"PLTE", bytes([0, 0, 0, 0, 0, 0, 255, 0, 0])), (b"tRNS", bytes([0, 128, 255]))]))
(root / "sixteen.png").write_bytes(png(3, 1, 16, 0, b"\0\0\0\x80\0\xff\xff"))
passes = [(0, 0, 8, 8), (4, 0, 8, 8), (0, 4, 4, 8), (2, 0, 4, 4), (0, 2, 2, 4), (1, 0, 2, 2), (0, 1, 1, 2)]
raw = bytearray()
for x0, y0, dx, dy in passes:
    for y in range(y0, 8, dy):
        raw.append(0)
        raw.extend(x * 16 + y for x in range(x0, 8, dx))
(root / "interlaced.png").write_bytes(png(8, 8, 8, 0, bytes(raw), interlace=1))
(root / "oversized.png").write_bytes(png(8192, 8192, 8, 0, b""))

image_files = {
    "META-INF/container.xml": files["META-INF/container.xml"],
    "EPUB/book.opf": '''<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="id"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:identifier id="id">urn:femto:fixture:images</dc:identifier><dc:title>Femto image sample</dc:title><dc:language>en</dc:language></metadata><manifest><item id="one" href="chapters/images.xhtml" media-type="application/xhtml+xml"/><item id="diagram" href="images/diagram.png" media-type="image/png"/><item id="gradient" href="images/gradient.jpg" media-type="image/jpeg"/><item id="tall" href="images/tall.png" media-type="image/png"/></manifest><spine><itemref idref="one"/></spine></package>''',
    "EPUB/chapters/images.xhtml": '''<html xmlns="http://www.w3.org/1999/xhtml"><head><title>Images</title></head><body><h1>Images on e-paper</h1><p>A PNG with alpha compositing and a JPEG gradient.</p><img src="../images/diagram.png" alt="Color and transparency"/><img src="../images/gradient.jpg" alt="JPEG gradient"/><p>A tall image fits a full page without cropping.</p><img src="../images/tall.png" alt="Tall checkerboard"/><p>The image-only page must retain its reading position.</p><img src="../images/missing.png" alt="Missing illustration"/></body></html>''',
    **{f"EPUB/images/{name}": (root / name).read_bytes() for name in ["diagram.png", "gradient.jpg", "tall.png"]},
}
with ZipFile(root / "images.epub", "w") as archive:
    archive.writestr(ZipInfo("mimetype"), "application/epub+zip", compress_type=ZIP_STORED)
    for path, data in image_files.items():
        archive.writestr(ZipInfo(path), data, compress_type=ZIP_DEFLATED)


navigation_files = {
    "META-INF/container.xml": files["META-INF/container.xml"],
    "EPUB/book.opf": '''<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="id"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:identifier id="id">urn:femto:navigation</dc:identifier><dc:title>Navigation fixture</dc:title></metadata><manifest><item id="one" href="text/one.xhtml" media-type="application/xhtml+xml"/><item id="two" href="text/two.xhtml" media-type="application/xhtml+xml"/><item id="nav" href="nav/toc.xhtml" properties="nav" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="one"/><itemref idref="two"/></spine></package>''',
    "EPUB/nav/toc.xhtml": '''<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body><nav epub:type="toc"><ol><li><a href="../text/one.xhtml#top">Beginning</a></li><li><a href="../text/two.xhtml#note">Footnote</a></li><li><a href="../text/two.xhtml#inline%20anchor">Inline target</a></li></ol></nav></body></html>''',
    "EPUB/text/one.xhtml": '''<html xmlns="http://www.w3.org/1999/xhtml"><body><h1 id="top">Links and footnotes</h1><p id="reference">Read <a href="two.xhtml#note">the <strong>first</strong> footnote</a>. <a href="https://example.org/">External</a>. <a href="two.xhtml#absent">Missing</a>.</p>''' + "".join(f"<p>{paragraph * 3}</p>" for _ in range(8)) + '</body></html>',
    "EPUB/text/two.xhtml": '<html xmlns="http://www.w3.org/1999/xhtml"><body><h1>Notes chapter</h1>' + "".join(f"<p>{paragraph * 3}</p>" for _ in range(14)) + '''<p id="note">The footnote target occurs after several pages. <a href="one.xhtml#reference">Return to reference</a>.</p><p>Before <span id="inline anchor">the inline target</span> after.</p><p id="A&amp;B">Escaped attribute target.</p></body></html>''',
}
with ZipFile(root / "navigation.epub", "w") as archive:
    archive.writestr(ZipInfo("mimetype"), "application/epub+zip", compress_type=ZIP_STORED)
    for path, data in navigation_files.items():
        archive.writestr(ZipInfo(path), data, compress_type=ZIP_DEFLATED)

tap_files = dict(navigation_files)
tap_files["EPUB/text/one.xhtml"] = '''<html xmlns="http://www.w3.org/1999/xhtml"><body>
<p dir="rtl">לפני <a href="two.xhtml#note">שלום עולם</a> אחרי</p>
<p><a href="two.xhtml#note">office affinity</a> ordinary</p>
<p><a href="two.xhtml#note"><img src="../images/diagram.png" alt="Linked image"/></a></p>
</body></html>'''
tap_files["EPUB/images/diagram.png"] = (root / "diagram.png").read_bytes()
with ZipFile(root / "tap-links.epub", "w") as archive:
    archive.writestr(ZipInfo("mimetype"), "application/epub+zip", compress_type=ZIP_STORED)
    for path, data in tap_files.items():
        archive.writestr(ZipInfo(path), data, compress_type=ZIP_DEFLATED)

metric_links = dict(navigation_files)
metric_links["EPUB/text/one.xhtml"] = '''<html xmlns="http://www.w3.org/1999/xhtml"><body>
<p><a href="two.xhtml#note">A&#x301;&#x301;&#x301;&#x301;&#x301;&#x301;&#x301;&#x301;gjpq</a></p>
<p><a href="two.xhtml#note" style="font-size: 8px">small link</a> <span style="font-size: 32px">BIG</span></p>
<p><a href="two.xhtml#note">next link</a></p>
</body></html>'''
with ZipFile(root / "metric-links.epub", "w") as archive:
    archive.writestr(ZipInfo("mimetype"), "application/epub+zip", compress_type=ZIP_STORED)
    for path, data in metric_links.items():
        archive.writestr(ZipInfo(path), data, compress_type=ZIP_DEFLATED)

indent_files = dict(files)
indent_files["EPUB/style.css"] = "body { text-indent: 2em; } .reset { text-indent: initial; } .percent { text-indent: 10%; }"
indent_files["EPUB/one.xhtml"] = '''<html xmlns="http://www.w3.org/1999/xhtml"><head><link rel="stylesheet" href="style.css"/></head><body>
<p>Inherited <span style="text-indent: 99px; font-size: 24px">inline override</span> stays in the paragraph.</p>
<div style="font-size: 24px; text-indent: 2em"><p style="font-size: 16px">Inherited computed length remains unchanged.</p></div>
<p class="reset">No indentation.</p><p class="percent">Percentage indentation.</p>
<p style="text-indent: 12px; text-indent: -3px">Negative value overrides the preceding declaration.</p>
<p style="text-indent: 8px; text-indent: inherit">Inherited reset.</p>
<p style="text-indent: 8px; text-indent: unset">Unset reset.</p>
<p dir="rtl" style="text-indent: 24px">שלום עולם שלום עולם שלום עולם שלום עולם שלום עולם שלום עולם</p>
<div style="font-size: 24px; text-indent: -2em"><p style="font-size: 16px">Inherited negative computed length.</p></div>
<p style="text-indent: -10%">Negative percentage indentation.</p>
</body></html>'''
with ZipFile(root / "indent.epub", "w") as archive:
    archive.writestr(ZipInfo("mimetype"), "application/epub+zip", compress_type=ZIP_STORED)
    for path, data in indent_files.items():
        archive.writestr(ZipInfo(path), data, compress_type=ZIP_DEFLATED)

language_files = dict(files)
language_files["EPUB/book.opf"] = files["EPUB/book.opf"].replace("Femto reading sample", "Femto language sample").replace("urn:femto:fixture:one", "urn:femto:fixture:language")
language_files["EPUB/one.xhtml"] = '''<html xmlns="http://www.w3.org/1999/xhtml"><body>
<p>extensive extensive</p>
<p lang="de-CH">Silbentrennung Silbentrennung</p>
<p xml:lang="fr">extraordinaire extraordinaire</p>
<p lang="">extensive extensive</p>
<p lang="es">extensive extensive</p>
<p>extensive extensive</p>
<p lang="de">Deutsch <span lang="fr">français <em xml:lang="">inconnu</em> français</span> Deutsch</p>
</body></html>'''
with ZipFile(root / "language.epub", "w") as archive:
    archive.writestr(ZipInfo("mimetype"), "application/epub+zip", compress_type=ZIP_STORED)
    for path, data in language_files.items():
        archive.writestr(ZipInfo(path), data, compress_type=ZIP_DEFLATED)

hyphenation_files = dict(files)
hyphenation_files["EPUB/book.opf"] = files["EPUB/book.opf"].replace("Femto reading sample", "Femto hyphenation sample").replace("urn:femto:fixture:one", "urn:femto:fixture:hyphenation")
hyphenation_files["EPUB/style.css"] = "body { hyphens: none } .manual { hyphens: manual } .automatic { hyphens: auto }"
hyphenation_files["EPUB/one.xhtml"] = '''<html xmlns="http://www.w3.org/1999/xhtml"><head><link rel="stylesheet" href="style.css"/></head><body>
<p>extensive ex&#173;tensive</p>
<p class="manual">extensive ex&#173;tensive</p>
<p class="automatic">extensive ex&#173;tensive</p>
<p class="automatic" style="hyphens: inherit">extensive ex&#173;tensive</p>
<p>of&#173;fice</p>
</body></html>'''
with ZipFile(root / "hyphenation.epub", "w") as archive:
    archive.writestr(ZipInfo("mimetype"), "application/epub+zip", compress_type=ZIP_STORED)
    for path, data in hyphenation_files.items():
        archive.writestr(ZipInfo(path), data, compress_type=ZIP_DEFLATED)

script_files = dict(files)
script_files["EPUB/book.opf"] = files["EPUB/book.opf"].replace("Femto reading sample", "Femto script shaping sample").replace("urn:femto:fixture:one", "urn:femto:fixture:scripts")
script_files["EPUB/one.xhtml"] = '''<html xmlns="http://www.w3.org/1999/xhtml"><body>
<h1>Mixed script shaping</h1>
<p>Serbian:</p><p lang="sr">office б office</p>
<p>Russian:</p><p lang="ru">office б office</p>
<p>Hebrew and Arabic:</p><p lang="ar">שלום سلام</p>
<p>Latin Ελληνικά Кириллица</p>
<p>Combining marks: á ά б́</p>
</body></html>'''
with ZipFile(root / "scripts.epub", "w") as archive:
    archive.writestr(ZipInfo("mimetype"), "application/epub+zip", compress_type=ZIP_STORED)
    for path, data in script_files.items():
        archive.writestr(ZipInfo(path), data, compress_type=ZIP_DEFLATED)

# Valid PNG headers with a damaged IDAT payload exercise deferred decode failure.
with ZipFile(root / "images.epub") as original, ZipFile(root / "corrupt-images.epub", "w") as archive:
    for name in original.namelist():
        data = original.read(name)
        if name == "EPUB/images/diagram.png":
            damaged = bytearray(data)
            offset = damaged.index(b"IDAT") + 4
            damaged[offset] ^= 1
            data = bytes(damaged)
        archive.writestr(ZipInfo(name), data, compress_type=ZIP_STORED if name == "mimetype" else ZIP_DEFLATED)

notes_files = dict(navigation_files)
notes_files["EPUB/book.opf"] = notes_files["EPUB/book.opf"].replace('<itemref idref="two"/>', '')
notes_files["EPUB/text/two.xhtml"] = '''<html xmlns="http://www.w3.org/1999/xhtml"><body><p id="note">A supplemental footnote outside the reading spine. <a href="one.xhtml#reference">Return to reference</a>.</p></body></html>'''
with ZipFile(root / "supplemental-notes.epub", "w") as archive:
    archive.writestr(ZipInfo("mimetype"), "application/epub+zip", compress_type=ZIP_STORED)
    for path, data in notes_files.items():
        archive.writestr(ZipInfo(path), data, compress_type=ZIP_DEFLATED)

noteref_files = dict(notes_files)
noteref_files["EPUB/text/one.xhtml"] = '''<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body>
<p><a href="two.xhtml#note" epub:type="other noteref">EPUB <strong>reference</strong></a></p>
<p><a role="doc-noteref" href="two.xhtml#note">ARIA reference</a><a href="two.xhtml#note">Ordinary link</a></p>
<p><a epub:type="notereference" href="two.xhtml#note">Not a marker</a></p>
<p><a type="noteref" href="two.xhtml#note">Bare type</a></p>
<p><a href="two.xhtml#absent" role="doc-noteref">Missing note</a></p>
<p><a href="https://example.org/" epub:type="noteref">External note</a></p>
<p><a epub:type="noteref" href="two.xhtml#note"><img src="missing.png" width="24" height="24" alt="Image reference"/></a></p>
</body></html>'''
with ZipFile(root / "noterefs.epub", "w") as archive:
    archive.writestr(ZipInfo("mimetype"), "application/epub+zip", compress_type=ZIP_STORED)
    for path, data in noteref_files.items():
        archive.writestr(ZipInfo(path), data, compress_type=ZIP_DEFLATED)

namespace_files = dict(notes_files)
namespace_files["EPUB/text/one.xhtml"] = '''<html xmlns="http://www.w3.org/1999/xhtml" xmlns:n="http://www.idpf.org/2007/ops" xmlns:epub="urn:wrong"><body>
<p><a n:type="noteref" href="two.xhtml#note">Inherited alias</a></p>
<p><a epub:type="noteref" href="two.xhtml#note">Wrong namespace</a></p>
<p><a type="noteref" href="two.xhtml#note">Bare attribute</a></p>
<div xmlns:n="urn:wrong"><p><a n:type="noteref" href="two.xhtml#note">Rebound alias</a></p>
<p><a xmlns:n="http://www.idpf.org/2007/ops" n:type="noteref" href="two.xhtml#note">Local declaration</a></p></div>
<p><a n:type="noteref" href="two.xhtml#note">Restored alias</a></p>
<p><a unknown:type="noteref" href="two.xhtml#note">Undeclared prefix</a></p>
<a xmlns:n="urn:wrong"/>
<p><a n:type="noteref" href="two.xhtml#note">After empty element</a></p>
</body></html>'''
with ZipFile(root / "note-namespaces.epub", "w") as archive:
    archive.writestr(ZipInfo("mimetype"), "application/epub+zip", compress_type=ZIP_STORED)
    for path, data in namespace_files.items():
        archive.writestr(ZipInfo(path), data, compress_type=ZIP_DEFLATED)

alignment_files = dict(files)
alignment_files["EPUB/style.css"] = "body { text-align: center } .right { text-align: right }"
alignment_files["EPUB/one.xhtml"] = '''<html xmlns="http://www.w3.org/1999/xhtml"><head><link rel="stylesheet" href="style.css"/></head><body>
<p>Centered <span style="text-align: left">inline</span></p>
<p class="right">Right</p><p style="text-align: left">Left</p>
<p style="text-align: end">End</p><p style="text-align: initial">Start</p>
<p class="right" style="text-align: inherit">Inherited</p>
</body></html>'''
with ZipFile(root / "alignment.epub", "w") as archive:
    archive.writestr(ZipInfo("mimetype"), "application/epub+zip", compress_type=ZIP_STORED)
    for path, data in alignment_files.items():
        archive.writestr(ZipInfo(path), data, compress_type=ZIP_DEFLATED)

direction_files = dict(files)
direction_files["EPUB/style.css"] = "body { direction: rtl } .left { direction: ltr }"
direction_files["EPUB/one.xhtml"] = '''<html xmlns="http://www.w3.org/1999/xhtml"><head><link rel="stylesheet" href="style.css"/></head><body>
<p>Latin first שלום</p>
<p class="left">שלום first Hebrew</p>
<p dir="ltr">Markup LTR</p>
<p dir="rtl" class="left">CSS overrides markup</p>
<p dir="ltr" style="direction: inherit">Inherit RTL</p>
<p style="direction: initial">Initial LTR</p>
<p dir="auto">Auto Latin שלום</p>
<p dir="rtl"><span dir="ltr" style="direction: ltr">Inline</span> stays RTL</p>
<div dir="ltr"><p>Container LTR</p><p style="direction: unset">Unset LTR</p></div>
<p dir="r&#116;l">Escaped RTL</p>
<p dir="invalid">Invalid inherits RTL</p>
<p style="direction: ltr; direction: nonsense">Invalid CSS keeps LTR</p>
</body></html>'''
with ZipFile(root / "direction.epub", "w") as archive:
    archive.writestr(ZipInfo("mimetype"), "application/epub+zip", compress_type=ZIP_STORED)
    for path, data in direction_files.items():
        archive.writestr(ZipInfo(path), data, compress_type=ZIP_DEFLATED)

whitespace_files = dict(files)
whitespace_files["EPUB/style.css"] = ".lines { white-space: pre-line } .preserve { white-space: pre-wrap }"
whitespace_files["EPUB/one.xhtml"] = '''<html xmlns="http://www.w3.org/1999/xhtml"><head><link rel="stylesheet" href="style.css"/></head><body><p>A <b>  bold </b> <i> word</i>&#32; <![CDATA[ tail]]></p><p><code>a  b
c</code></p><pre>  first

    second <b>  bold</b></pre><p class="lines">first
  second <span>  word</span>
third</p><p class="preserve">a  <span> b</span>
c</p><pre style="white-space: normal">a  b
c</pre><p id="linked">A <a href="#linked">  link</a>  tail</p></body></html>'''
with ZipFile(root / "whitespace.epub", "w") as archive:
    archive.writestr(ZipInfo("mimetype"), "application/epub+zip", compress_type=ZIP_STORED)
    for path, data in whitespace_files.items():
        archive.writestr(ZipInfo(path), data, compress_type=ZIP_DEFLATED)

tab_size_files = dict(files)
tab_size_files["EPUB/style.css"] = "body { tab-size: 4; white-space: pre-wrap } .pixels { tab-size: 40px }"
tab_size_files["EPUB/one.xhtml"] = '''<html xmlns="http://www.w3.org/1999/xhtml"><head><link rel="stylesheet" href="style.css"/></head><body><p>A\tB</p><p class="pixels">A\tB</p><p style="tab-size: 0">A\tB</p><p style="tab-size: 2.5">A\tB</p><p style="tab-size: initial">A\tB</p><p class="pixels" style="tab-size: inherit">A\tB</p><p style="tab-size: unset">A\tB</p><p style="tab-size: 3; tab-size: -1">A\tB</p><p>A<span style="tab-size: 40px">\tB</span><span style="tab-size: 0">\tC</span></p></body></html>'''
with ZipFile(root / "tab-size.epub", "w") as archive:
    archive.writestr(ZipInfo("mimetype"), "application/epub+zip", compress_type=ZIP_STORED)
    for path, data in tab_size_files.items():
        archive.writestr(ZipInfo(path), data, compress_type=ZIP_DEFLATED)

# Original fixtures for the vendored book API regression tests.
api_files = {
    "META-INF/container.xml": files["META-INF/container.xml"],
    "EPUB/book.opf": '''<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="id"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:identifier id="id">urn:femto:book-api</dc:identifier><dc:title>Femto book API fixture</dc:title><dc:language>en</dc:language></metadata><manifest><item id="chapter" href="xhtml/chapter.xhtml" media-type="application/xhtml+xml"/><item id="nav" href="xhtml/nav.xhtml" media-type="application/xhtml+xml" properties="nav"/><item id="cover" href="images/cover.jpg" media-type="image/jpeg" properties="cover-image"/><item id="style" href="styles/book.css" media-type="text/css"/><item id="font" href="fonts/DejaVuSerif.ttf" media-type="font/ttf"/></manifest><spine><itemref idref="chapter"/></spine></package>''',
    "EPUB/xhtml/nav.xhtml": '''<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body><nav epub:type="toc"><ol><li><a href="chapter.xhtml#start">Original chapter</a></li></ol></nav></body></html>''',
    "EPUB/xhtml/chapter.xhtml": '''<html xmlns="http://www.w3.org/1999/xhtml"><head><link rel="stylesheet" href="../styles/book.css"/></head><body><h1 id="start">Original chapter</h1><p>This original text exercises streamed extraction, resource limits, styled runs, and font selection. Unicode text includes café and שלום.</p><p>Another paragraph contains <strong>bold</strong> and <em>italic</em> spans.</p><img src="../images/cover.jpg" alt="Synthetic gradient"/></body></html>''',
    "EPUB/styles/book.css": '''@font-face { font-family: "FemtoFixture"; src: url("../fonts/DejaVuSerif.ttf") format("truetype"); } body { font-family: "FemtoFixture"; font-size: 16px; } strong { font-weight: bold; } em { font-style: italic; }''',
    "EPUB/fonts/DejaVuSerif.ttf": (root / "DejaVuSerif.ttf").read_bytes(),
    "EPUB/fonts/LICENSE.txt": (root / "DejaVu-LICENSE.txt").read_bytes(),
    "EPUB/images/cover.jpg": (root / "gradient.jpg").read_bytes(),
}
assert len(api_files["EPUB/images/cover.jpg"]) > 1024
for legacy in [False, True]:
    contents = dict(api_files)
    if legacy:
        contents["EPUB/book.opf"] = contents["EPUB/book.opf"].replace('version="3.0"', 'version="2.0"').replace('</metadata>', '<meta name="cover" content="cover"/></metadata>').replace(' properties="cover-image"', '').replace('images/cover.jpg', 'images/original_cover.jpg')
        contents["EPUB/book.opf"] = contents["EPUB/book.opf"].replace('href="xhtml/nav.xhtml" media-type="application/xhtml+xml" properties="nav"', 'href="toc.ncx" media-type="application/x-dtbncx+xml"').replace('<spine>', '<spine toc="nav">').replace('media-type="font/ttf"', 'media-type="application/x-font-ttf"')
        contents.pop("EPUB/xhtml/nav.xhtml")
        contents["EPUB/toc.ncx"] = '''<ncx xmlns="http://www.daisy.org/z3986/2005/ncx/" version="2005-1"><head><meta name="dtb:uid" content="urn:femto:book-api"/><meta name="dtb:depth" content="1"/><meta name="dtb:totalPageCount" content="0"/><meta name="dtb:maxPageNumber" content="0"/></head><docTitle><text>Femto book API fixture</text></docTitle><navMap><navPoint id="start" playOrder="1"><navLabel><text>Original chapter</text></navLabel><content src="xhtml/chapter.xhtml#start"/></navPoint></navMap></ncx>'''
        contents["EPUB/images/original_cover.jpg"] = contents.pop("EPUB/images/cover.jpg")
        contents["EPUB/xhtml/chapter.xhtml"] = contents["EPUB/xhtml/chapter.xhtml"].replace('images/cover.jpg', 'images/original_cover.jpg')
    with ZipFile(root / ("book-api-legacy-cover.epub" if legacy else "book-api.epub"), "w") as archive:
        archive.writestr(ZipInfo("mimetype"), "application/epub+zip", compress_type=ZIP_STORED)
        for path, data in contents.items():
            archive.writestr(ZipInfo(path), data, compress_type=ZIP_DEFLATED)

embedded_css = dict(files)
embedded_css["EPUB/book.opf"] = embedded_css["EPUB/book.opf"].replace('</manifest>', '<item id="after" href="after.css" media-type="text/css"/></manifest>')
embedded_css["EPUB/style.css"] = "p { text-indent: 8px; text-align: left; }"
embedded_css["EPUB/after.css"] = "p { text-indent: 36px; } .middle { text-align: right; }"
embedded_css["EPUB/one.xhtml"] = '''<html xmlns="http://www.w3.org/1999/xhtml"><head>
<link rel="stylesheet" href="style.css"/>
<style type="text/css"><![CDATA[p { text-indent: 24px; } .middle { text-align: center; }]]></style>
<link rel="stylesheet" href="after.css"/>
<style media="screen">.last { text-indent: 10%; } p { hyphens: none; }</style>
<style media="print">p { text-indent: 100px; }</style>
<style type="application/json">not a stylesheet</style>
</head><body><p style="text-indent: initial">Inline reset.</p><p class="middle">Linked sheet after embedded sheet.</p><p class="last">Embedded sheet after linked sheet.</p></body></html>'''
with ZipFile(root / "embedded-css.epub", "w") as archive:
    archive.writestr(ZipInfo("mimetype"), "application/epub+zip", compress_type=ZIP_STORED)
    for path, data in embedded_css.items():
        archive.writestr(ZipInfo(path), data, compress_type=ZIP_DEFLATED)

list_files = dict(files)
list_files["EPUB/one.xhtml"] = '''<html xmlns="http://www.w3.org/1999/xhtml"><body><ol>
<li id="first">First <a href="#first">linked item</a>.<p>Continuation paragraph.</p><ul><li>Nested bullet.</li></ul>Parent tail.</li>
<li>Second numbered item.</li></ol><ul dir="rtl"><li>שלום עולם שלום עולם</li></ul>
<ol><li>''' + paragraph * 40 + '''</li></ol></body></html>'''
with ZipFile(root / "lists.epub", "w") as archive:
    archive.writestr(ZipInfo("mimetype"), "application/epub+zip", compress_type=ZIP_STORED)
    for path, data in list_files.items():
        archive.writestr(ZipInfo(path), data, compress_type=ZIP_DEFLATED)
for depth in [16, 17]:
    nested = dict(files)
    nested["EPUB/one.xhtml"] = '<html xmlns="http://www.w3.org/1999/xhtml"><body>' + '<ul><li>' * depth + 'Deep item.' + '</li></ul>' * depth + '</body></html>'
    with ZipFile(root / f"lists-depth-{depth}.epub", "w") as archive:
        archive.writestr(ZipInfo("mimetype"), "application/epub+zip", compress_type=ZIP_STORED)
        for path, data in nested.items():
            archive.writestr(ZipInfo(path), data, compress_type=ZIP_DEFLATED)
numbered = dict(files)
numbered["EPUB/one.xhtml"] = '''<html xmlns="http://www.w3.org/1999/xhtml"><body>
<ol start="7"><li>Seven.</li><li value="12">Twelve.</li><li>Thirteen.<ol start="-2"><li>Minus two.</li><li value="0">Zero.</li><li>One.</li></ol></li><li>Fourteen.</li></ol>
<ul start="7"><li value="9">Still a bullet.</li></ul>
<ol start="nonsense"><li value="1.5">Invalid attributes retain ordinary numbering.</li><li value="2147483648">Overflowing attribute is ignored.</li></ol>
<ol start="&#43;4"><li>Escaped plus.</li><li value=" -3 ">Negative override.</li><li>Minus two.</li></ol>
<ol start="-2147483648"><li>Minimum signed attribute.</li></ol><ol start="2147483647"><li>Maximum signed attribute.<p>Continuation with the same wide marker space.</p></li><li>Next counter uses wider storage.</li></ol>
</body></html>'''
with ZipFile(root / "list-numbers.epub", "w") as archive:
    archive.writestr(ZipInfo("mimetype"), "application/epub+zip", compress_type=ZIP_STORED)
    for path, data in numbered.items():
        archive.writestr(ZipInfo(path), data, compress_type=ZIP_DEFLATED)
reversed_lists = dict(files)
reversed_lists["EPUB/one.xhtml"] = '''<html xmlns="http://www.w3.org/1999/xhtml"><body>
<ol reversed="reversed"><li>Three.<ol reversed=""><li>Nested two.</li><li>Nested one.</li></ol></li><li>Two.</li><li>One.</li></ol>
<ol start="-1" reversed="false"><li>Negative start.</li><li value="4">Override four.</li><li>Three after override.</li></ol>
<ol reversed=""><li/><li>Empty item counted before this two.</li><li>One after empty item.</li></ol>
<ol reversed="" start="invalid"><li>Fallback two.</li><li>Fallback one.</li></ol>
<ol reversed="" start="-2147483648"><li>Minimum start.</li><li>Below minimum attribute in wider counter.</li></ol>
<ul reversed=""><li>Bullet unaffected.</li></ul>
</body></html>'''
with ZipFile(root / "list-reversed.epub", "w") as archive:
    archive.writestr(ZipInfo("mimetype"), "application/epub+zip", compress_type=ZIP_STORED)
    for path, data in reversed_lists.items():
        archive.writestr(ZipInfo(path), data, compress_type=ZIP_DEFLATED)
typed_lists = dict(files)
typed_lists["EPUB/one.xhtml"] = '''<html xmlns="http://www.w3.org/1999/xhtml"><body>
<ol type="a" start="26"><li>Last single letter.</li><li>First double letter.<p>Continuation.</p><ol type="I" start="4"><li>Nested Roman.</li></ol>Parent tail.</li><li type="A" value="52">Upper override.</li><li>Lower style restored.</li><li value="0">Zero fallback.</li><li>Positive again.</li></ol>
<ol type="I" start="3999"><li>Largest Roman.</li><li>Decimal fallback.</li><li type="i" value="49">Lower Roman override.</li><li value="9" type="i">Opposite attribute order.</li><li>Parent upper restored.</li><li type="1">Decimal override.</li></ol>
<ol reversed="" type="&#97;"><li>Reverse b.</li><li>Reverse a.</li></ol>
<ol type="i" start="-1"><li>Negative fallback.</li><li>Zero fallback.</li><li>Roman one.</li></ol>
<ol type="invalid"><li type=" A ">Invalid styles ignored.</li></ol>
<ol type="A" start="702"><li>Upper ZZ.</li><li>Upper AAA.</li><li value="2147483647">Large alpha.</li></ol>
</body></html>'''
with ZipFile(root / "list-types.epub", "w") as archive:
    archive.writestr(ZipInfo("mimetype"), "application/epub+zip", compress_type=ZIP_STORED)
    for path, data in typed_lists.items():
        archive.writestr(ZipInfo(path), data, compress_type=ZIP_DEFLATED)
em_tabs = dict(files)
em_tabs["EPUB/style.css"] = 'body { font-size: 24px; white-space: pre-wrap; tab-size: 2em; } .small { font-size: 16px; } .own { font-size: 16px; tab-size: 2em; } .override { tab-size: 10px; }'
em_tabs["EPUB/one.xhtml"] = '''<html xmlns="http://www.w3.org/1999/xhtml"><head><link rel="stylesheet" href="style.css"/></head><body><p>A\tB</p><p class="small">A\tB</p><p class="own">A\tB</p><p class="override" style="tab-size: inherit; font-size: 16px">A\tB</p><p style="tab-size: unset; font-size: 16px">A\tB</p><p style="tab-size: initial">A\tB</p><p style="tab-size: 0em">A\tB</p><p class="small">A<span style="font-size: 32px">\tB</span><span style="font-size: 32px; tab-size: 2em">\tC</span></p><p dir="rtl" class="small">א\tב</p></body></html>'''
with ZipFile(root / "tab-em.epub", "w") as archive:
    archive.writestr(ZipInfo("mimetype"), "application/epub+zip", compress_type=ZIP_STORED)
    for path, data in em_tabs.items():
        archive.writestr(ZipInfo(path), data, compress_type=ZIP_DEFLATED)

css_lists = dict(files)
css_lists["EPUB/style.css"] = "body { list-style-type: square; } .roman { list-style-type: upper-roman; } .round { list-style-type: circle; }"
css_lists["EPUB/one.xhtml"] = '''<html xmlns="http://www.w3.org/1999/xhtml"><head><link rel="stylesheet" href="style.css"/></head><body>
<ol class="roman" type="a" start="4"><li>Roman CSS overrides HTML.</li><li value="8" type="I" style="list-style-type: lower-alpha">Item override.</li><li style="list-style-type: none">Hidden marker still advances.</li><li>Roman resumes.</li></ol>
<ul class="round"><li>Circle bullet.<ul><li>Nested default disc.</li></ul></li><li style="list-style-type: upper-alpha">Alphabetic unordered item.</li></ul>
<ol style="list-style-type: inherit"><li>Inherited square.</li></ol>
<ol reversed="reversed" style="list-style-type: lower-latin"><li>Reversed beta.</li><li>Reversed alpha.</li></ol>
<ol style="list-style-type: upper-roman"><li style="list-style-type: initial">Initial disc.</li><li style="list-style-type: unset">Unset numeral.</li><li style="list-style-type: none"/><li>Empty item advances.</li></ol>
</body></html>'''
with ZipFile(root / "list-css.epub", "w") as archive:
    archive.writestr(ZipInfo("mimetype"), "application/epub+zip", compress_type=ZIP_STORED)
    for path, data in css_lists.items():
        archive.writestr(ZipInfo(path), data, compress_type=ZIP_DEFLATED)

# Style boundaries must not reset bracket script context.
with ZipFile(root / "scripts.epub") as source, ZipFile(root / "styled-brackets.epub", "w") as archive:
    for info in source.infolist():
        data = source.read(info.filename)
        if info.filename == "EPUB/book.opf":
            data = data.replace(b"Femto script shaping sample", b"Femto styled bracket sample").replace(b"urn:femto:fixture:scripts", b"urn:femto:fixture:styled-brackets")
        elif info.filename == "EPUB/one.xhtml":
            data = '''<html xmlns="http://www.w3.org/1999/xhtml"><body>
<h1 id="start">Styled bracket context</h1>
<p>Latin (<em>Ελληνικά</em>) Latin.</p>
<p>Latin <strong>(</strong>Ελληνικά<span style="font-size:1.2em">)</span> Latin.</p>
<p><em>(</em>α <strong>б</strong><em>)</em></p>
<p lang="sr">office (<span lang="ru">б</span> <strong>б</strong>) office</p>
<p>α [<em>Latin (</em>Кириллица<em>) Latin</em>] α.</p>
<p lang="ar">שלום (<em>سلام</em>) שלום</p>
</body></html>'''.encode()
        archive.writestr(ZipInfo(info.filename), data, compress_type=info.compress_type)

# Image max constraints use the fixed reader content box.
with ZipFile(root / "images.epub") as source, ZipFile(root / "image-limits.epub", "w") as archive:
    for info in source.infolist():
        data = source.read(info.filename)
        if info.filename == "EPUB/book.opf":
            data = data.replace(b"Femto image sample", b"Femto image limit sample").replace(b"urn:femto:fixture:images", b"urn:femto:fixture:image-limits")
            data = data.replace(b"</manifest>", b'<item id="css" href="styles/limits.css" media-type="text/css"/></manifest>')
        elif info.filename == "EPUB/chapters/images.xhtml":
            data = '''<html xmlns="http://www.w3.org/1999/xhtml"><head><link rel="stylesheet" href="../styles/limits.css"/><style>.zero { max-width: 0 }</style></head><body>
<h1>Image constraints</h1>
<div style="max-width:1px"><img src="../images/gradient.jpg" alt="Natural"/></div>
<img class="half" src="../images/gradient.jpg" alt="Half content width"/>
<img style="max-width:80px; max-height:30px" src="../images/gradient.jpg" alt="Both constraints"/>
<img style="max-height:25%" src="../images/gradient.jpg" alt="Quarter content height"/>
<img class="half" style="max-width:none" src="../images/gradient.jpg" alt="Reset"/>
<img class="zero" src="../images/gradient.jpg" alt="Hidden width"/>
<img style="max-width:33.9px" src="../images/gradient.jpg" alt="Fractional pixels"/>
<img style="max-width:999999%; max-height:999999px" src="../images/gradient.jpg" alt="Viewport clamp"/>
<img class="half" width="80" height="60" src="../images/gradient.jpg" alt="HTML hints"/>
<img style="max-height:0" src="../images/gradient.jpg" alt="Hidden height"/>
<p>Reading continues after zero-size images.</p>
</body></html>'''.encode()
        archive.writestr(ZipInfo(info.filename), data, compress_type=info.compress_type)
    archive.writestr(ZipInfo("EPUB/styles/limits.css"), "img { max-height:200px } .half { max-width:50% } .zero { max-width:100px }", compress_type=ZIP_DEFLATED)

semantic_notes = dict(notes_files)
semantic_notes["EPUB/text/one.xhtml"] = '''<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body><p>
<a epub:type="noteref" href="two.xhtml#long">Long note</a>;
<a epub:type="noteref" href="two.xhtml#inline">Inline note</a>;
<a epub:type="noteref" href="two.xhtml#wrong">Unmarked target</a>.
</p></body></html>'''
semantic_notes["EPUB/text/two.xhtml"] = '''<html xmlns="http://www.w3.org/1999/xhtml" xmlns:n="http://www.idpf.org/2007/ops"><body>
<h1>EXCLUDED_HEADING</h1><section n:type="endnotes"><p>EXCLUDED_INTRO</p>
<aside n:type="endnote" id="long"><p>FIRST_NOTE_START</p>''' + ''.join('<p>Selected note text with accents café and shaping office. ' + paragraph * 2 + '</p>' for _ in range(12)) + '''<p>FIRST_NOTE_END</p></aside>
<aside role="doc-footnote"><p>EXCLUDED_ADJACENT_NOTE</p></aside></section>
<p>EXCLUDED_PREFIX <span role="doc-footnote" id="inline">INLINE_NOTE café <strong>styled content</strong></span> EXCLUDED_SUFFIX</p>
<aside xmlns:n="urn:wrong" n:type="footnote" id="wrong"><p>UNMARKED_TARGET</p></aside>
<p>EXCLUDED_TRAILER</p></body></html>'''
with ZipFile(root / "semantic-notes.epub", "w") as archive:
    archive.writestr(ZipInfo("mimetype"), "application/epub+zip", compress_type=ZIP_STORED)
    for path, data in semantic_notes.items():
        archive.writestr(ZipInfo(path), data, compress_type=ZIP_DEFLATED)

note_context = dict(semantic_notes)
note_context["EPUB/text/one.xhtml"] = '''<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body><p><a epub:type="noteref" href="two.xhtml#ltr">English note</a> <a epub:type="noteref" href="two.xhtml#rtl">Hebrew note</a></p></body></html>'''
note_context["EPUB/text/two.xhtml"] = '<html xmlns="http://www.w3.org/1999/xhtml"><body><p>עברית לפני <span role="doc-footnote" id="ltr">' + ('English note context. ' * 80).rstrip() + '</span> עברית אחרי</p><p>English before <span role="doc-footnote" id="rtl">' + ('שלום עולם ' * 160).rstrip() + '</span> English after</p></body></html>'
with ZipFile(root / "note-context.epub", "w") as archive:
    archive.writestr(ZipInfo("mimetype"), "application/epub+zip", compress_type=ZIP_STORED)
    for path, data in note_context.items():
        archive.writestr(ZipInfo(path), data, compress_type=ZIP_DEFLATED)

with ZipFile(root / "images.epub") as source, ZipFile(root / "image-dimensions.epub", "w") as archive:
    for info in source.infolist():
        data = source.read(info.filename)
        if info.filename == "EPUB/chapters/images.xhtml":
            data = '''<html xmlns="http://www.w3.org/1999/xhtml"><head><style>.half { width:120px; height:auto } .half { width:50% }</style></head><body>
<img class="half" src="../images/gradient.jpg"/>
<img style="height:30px" src="../images/gradient.jpg"/>
<img style="width:80px;height:20px" src="../images/gradient.jpg"/>
<img style="width:640px" src="../images/gradient.jpg"/>
<img height="60" style="width:100px;height:auto" src="../images/gradient.jpg"/>
<img width="80" style="width:auto;height:40px" src="../images/gradient.jpg"/>
<img style="width:80px;height:80px;max-width:40px" src="../images/gradient.jpg"/>
<img style="width:0" src="missing.png" alt="Hidden missing asset"/>
<div style="width:5px;height:5px"><img src="../images/gradient.jpg"/></div>
<img class="half" style="width:initial;height:unset" src="../images/gradient.jpg"/>
<p>Reading continues.</p></body></html>'''.encode()
        archive.writestr(ZipInfo(info.filename), data, compress_type=info.compress_type)

# Image minimum dimensions, resets, non-inheritance, and conflicting constraints.
with ZipFile(root / "images.epub") as source, ZipFile(root / "image-minimums.epub", "w") as archive:
    for info in source.infolist():
        data = source.read(info.filename)
        if info.filename == "EPUB/chapters/images.xhtml":
            data = '''<html xmlns="http://www.w3.org/1999/xhtml"><head><style>.minimum { min-width:400px } .minimum { min-width:420px } .reset { min-width:400px }</style></head><body>
<img style="min-width:400px" src="../images/gradient.jpg"/>
<img style="min-height:300px" src="../images/gradient.jpg"/>
<img style="min-width:400px;max-height:250px" src="../images/gradient.jpg"/>
<img style="min-width:400px;max-width:80px" src="../images/gradient.jpg"/>
<img style="width:0;min-width:80px" src="../images/gradient.jpg"/>
<img style="width:80px;height:20px;min-height:40px" src="../images/gradient.jpg"/>
<img style="min-width:100%;min-height:50%" src="../images/gradient.jpg"/>
<div style="min-width:400px;min-height:500px"><img src="../images/gradient.jpg"/></div>
<img class="reset" style="min-width:initial" src="../images/gradient.jpg"/>
<img class="minimum" style="min-width:400px;min-width:-1px;min-height:NaNpx" src="../images/gradient.jpg"/>
<img style="width:0" src="../images/missing.jpg" alt="Hidden"/>
</body></html>'''.encode()
        archive.writestr(info, data)
