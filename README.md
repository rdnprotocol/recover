# MetaRecover — устгагдсан файл сэргээх програм

Устгагдсан файлыг сэргээх Windows desktop програм. Rust engine + Tauri GUI, бүрэн Монгол хэлтэй.

**[⬇ Татаж авах](https://rdnprotocol.github.io/recover/)** · [Releases](https://github.com/rdnprotocol/recover/releases/latest)

## Боломжууд

| | |
|---|---|
| **Хурдан скан** | NTFS, FAT12/16/32, exFAT дээр устгагдсан файлыг **нэр, хавтас, огноотой нь** олно |
| **Гүн скан** | Format хийсэн, RAW болсон, хуваалт нь устсан дискнээс файлын signature-ээр хайна |
| **Дэмжих формат (гүн скан)** | JPG, PNG, GIF, BMP, WEBP, PDF, DOCX/XLSX/PPTX, ODT/ODS/ODP, DOC/XLS/PPT, ZIP, 7Z, MP4/MOV/HEIC/3GP, MKV/WEBM, AVI, WAV, MP3 |
| **Хэсэгчилсэн файл** | NTFS data run, exFAT FAT chain-аар fragmented файлыг бүрэн угсарна |
| **Чанарын үнэлгээ** | Cluster дахин ашиглагдсан эсэхийг bitmap/FAT-аар шалгаж "Сайн / Тодорхойгүй / Гэмтсэн" гэж тэмдэглэнэ |
| **Preview** | Зураг, текст, бусад файлын hex — сэргээхээс өмнө харна |
| **Диск** | Боть (C:, D:, USB), физик диск (MBR + GPT хуваалт), disk image (.img/.dd) |
| **Аюулгүй байдал** | Зөвхөн уншина. Сэргээж буй дискэн дээрээ хадгалахыг хориглоно |

## Бүтэц

```
metarecover/
├── core/         Rust engine (std + serde л ашиглана)
│   ├── device.rs   sector-aligned raw уншилт, bad sector-ийг 0-ээр нөхнө
│   ├── volume.rs   FS таних, MBR/GPT хуваалт
│   ├── ntfs.rs     $MFT бичлэг бүрийг уншина, устгагдсан = in-use flag 0
│   ├── fat.rs      0xE5 entry, LFN (урт нэр) сэргээх
│   ├── exfat.rs    InUse bit 0 entry set, allocation bitmap
│   ├── carve.rs    формат бүрийн бүтцийг алхаж файлын яг төгсгөлийг олно
│   └── recover.rs  файл бичих, огноо сэргээх
├── cli/          `metarecover` командын мөрийн хэрэгсэл
├── app/          Tauri 2 GUI (src-tauri + ui)
└── tests/        Linux дээрх end-to-end тест
```

## Windows дээр build хийх

1. Суулгах:
   - [Rust](https://rustup.rs) (MSVC toolchain)
   - Visual Studio Build Tools — "Desktop development with C++"
   - WebView2 (Windows 10/11-д аль хэдийн байдаг)
   - Tauri CLI: `cargo install tauri-cli --version "^2"`
2. Installer гаргах:
   ```powershell
   cd app\src-tauri
   cargo tauri build
   ```
   Гарах файл: `app\src-tauri\target\release\bundle\nsis\MetaRecover_0.1.0_x64-setup.exe` болон `msi\...msi`
3. Зөвхөн CLI:
   ```powershell
   cargo build --release -p metarecover-cli
   ```

> Програм эхлэхдээ **Administrator эрх** асууна (UAC). Энэ нь `app.manifest`-д тохируулагдсан — дискийг шууд уншихад заавал хэрэгтэй.
> `cargo tauri dev` ажиллуулахдаа терминалаа "Run as administrator"-аар нээнэ үү.

## CLI хэрэглээ

```powershell
metarecover devices
metarecover scan \\.\E:                       # USB-ийн устгагдсан файлууд
metarecover scan \\.\E: --deep --json r.json  # гүн скан, үр дүнг JSON-оор
metarecover recover \\.\E: --out D:\Recovered --ext jpg,docx,pdf
metarecover recover \\.\PhysicalDrive1 --out D:\Recovered --deep
metarecover recover disk.img --out out --ids 3,7,12
```

## Тест

Linux (root) дээр бодит FAT32/FAT16/NTFS/exFAT image үүсгэж, файл устгаад, сэргээж, SHA-256-аар харьцуулна:

```bash
cargo build --release
sudo tests/run_tests.sh
```

Хөгжүүлэлтийн явцад шалгасан үр дүн:

| Тест | Үр дүн |
|---|---|
| FAT32 / FAT16 — устгасан файл, устгасан хавтас, кирилл урт нэр | 7/7 байт-байтаар ижил |
| Format хийсэн FAT32 (гүн скан) | бүх файл олдсон |
| 11 форматын raw carving | 11/11 |
| NTFS — 6 хэсэгт хуваагдсан файл, MFT доторх жижиг файл | бүгд OK |
| exFAT — FAT chain устсан fragmented файл | OK |
| MBR болон GPT хуваалттай диск | 30/30 |
| GUI end-to-end (скан → preview → сэргээх) | OK |

## Мэдэж байх хязгаарлалт

- **SSD + TRIM:** Windows устгасан файлын блокийг SSD-д "хоосон" гэж мэдэгддэг тул ихэнх тохиолдолд өгөгдөл бодитоор арилдаг. Энэ асуудал бүх сэргээх програмд адилхан. HDD, USB flash, SD карт дээр сэргээлт хамгийн сайн ажиллана.
- **FAT дээрх fragmented файл:** FAT устгахдаа cluster chain-ийг арилгадаг. Програм эхлээд дараалсан cluster-ийг, тэгээд чөлөөтэй cluster-уудыг угсарч үзнэ. Үр дүн нь "Тодорхойгүй" гэж тэмдэглэгдэнэ.
- **Нэрийн эхний үсэг:** урт нэргүй (8.3) FAT файлын эхний үсэг устгах үед алдагддаг тул `_` болж харагдана.
- **NTFS шахсан/шифрлэсэн файл:** шахсан файлыг "Гэмтсэн" гэж тэмдэглэнэ (LZNT1 тайлах функц хараахан хийгдээгүй), EFS шифрлэсэн файлыг алгасна.
- Гүн скан нь файлын нэр, хавтсыг олж чадахгүй (`f000123456.jpg` хэлбэрээр нэрлэнэ).
- Одоогоор GUI нь Windows-д зориулагдсан. Engine нь Linux/macOS дээр ч ажилладаг.

## Цаашид нэмж болох зүйлс

NTFS LZNT1 тайлах, $LogFile/$UsnJrnl-ээс нэр сэргээх, RAR/HEIC preview, скан үр дүнг хадгалж дараа үргэлжлүүлэх, RAID/BitLocker дэмжлэг, лиценз ба идэвхжүүлэлт (QPay).

## Лиценз

MIT — [LICENSE](LICENSE)
