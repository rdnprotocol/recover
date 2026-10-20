"""Generate sample files of every carved format for recovery tests."""
import io, os, struct, sys, wave, zipfile, random
from PIL import Image

out = sys.argv[1]
os.makedirs(out, exist_ok=True)
random.seed(42)

def noise_img(w, h):
    img = Image.new("RGB", (w, h))
    px = img.load()
    for y in range(h):
        for x in range(w):
            px[x, y] = ((x * 7 + y * 3) % 256, (x * y) % 256, random.randint(0, 255))
    return img

noise_img(640, 480).save(f"{out}/photo1.jpg", quality=92)
noise_img(800, 600).save(f"{out}/зураг_2.jpg", quality=90)
noise_img(300, 200).save(f"{out}/diagram.png")
noise_img(120, 90).convert("P").save(f"{out}/anim.gif")
noise_img(200, 150).save(f"{out}/scan.bmp")

# PDF with an incremental update (two %%EOF markers)
body = b"%PDF-1.4\n1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj\n2 0 obj<</Type/Pages/Count 0/Kids[]>>endobj\n"
body += b"3 0 obj<</Length 4000>>stream\n" + bytes(random.getrandbits(8) for _ in range(4000)) + b"\nendstream endobj\n"
body += b"trailer<</Root 1 0 R>>\n%%EOF\n"
body += b"4 0 obj<</Producer(MetaRecover test)>>endobj\ntrailer<</Root 1 0 R/Prev 9>>\n%%EOF\n"
open(f"{out}/албан_бичиг.pdf", "wb").write(body)

# DOCX-like OOXML zip (stored + deflated parts)
with zipfile.ZipFile(f"{out}/Тайлан 2026.docx", "w", zipfile.ZIP_DEFLATED) as z:
    z.writestr("[Content_Types].xml", '<?xml version="1.0"?><Types/>')
    z.writestr("word/document.xml", "<w:document>" + "Монгол текст " * 5000 + "</w:document>")
    z.writestr("word/media/image1.bin", bytes(random.getrandbits(8) for _ in range(150000)))
with zipfile.ZipFile(f"{out}/budget.xlsx", "w", zipfile.ZIP_DEFLATED) as z:
    z.writestr("[Content_Types].xml", "<Types/>")
    z.writestr("xl/workbook.xml", "<workbook/>" * 2000)

# WAV
with wave.open(f"{out}/voice.wav", "wb") as w:
    w.setnchannels(1); w.setsampwidth(2); w.setframerate(16000)
    w.writeframes(bytes(random.getrandbits(8) for _ in range(160000)))

# MP3: ID3v2 tag + MPEG1 Layer III 128kbps 44.1kHz frames (417 bytes, no padding)
tag = b"ID3\x03\x00\x00" + bytes([0, 0, 0, 20]) + b"TIT2\x00\x00\x00\x0a\x00\x00\x00Duu test"
tag = tag[:30].ljust(30, b"\x00")
frames = b""
for i in range(200):
    hdr = bytes([0xFF, 0xFB, 0x90, 0x64])
    frames += hdr + bytes(random.getrandbits(8) for _ in range(417 - 4))
open(f"{out}/song.mp3", "wb").write(tag + frames)

# MP4: ftyp + mdat + moov
def box(t, payload):
    return struct.pack(">I", 8 + len(payload)) + t + payload
mp4 = box(b"ftyp", b"isom\x00\x00\x02\x00isomiso2mp41")
mp4 += box(b"mdat", bytes(random.getrandbits(8) for _ in range(300000)))
mp4 += box(b"moov", box(b"mvhd", b"\x00" * 100))
open(f"{out}/clip.mp4", "wb").write(mp4)

# small text file (NTFS resident data)
open(f"{out}/тэмдэглэл.txt", "w", encoding="utf-8").write("Нууц үг биш, зүгээр тэмдэглэл.\n")

for f in sorted(os.listdir(out)):
    print(f, os.path.getsize(os.path.join(out, f)))
