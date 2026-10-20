#!/usr/bin/env bash
# End-to-end recovery tests on Linux (needs root, dosfstools, mtools, ntfs-3g,
# exfatprogs, exfat-fuse, python3-pil). Builds disk images, deletes files,
# recovers them with the CLI and compares SHA-256 against the originals.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
BIN="$HERE/../target/release/metarecover"
W=${1:-$(mktemp -d)}
export LANG=C.UTF-8 MTOOLS_SKIP_CHECK=1
mkdir -p "$W"; cd "$W"; rm -rf src out_* *.img mnt; mkdir -p mnt
python3 "$HERE/make_fixtures.py" src >/dev/null
sha256sum src/* > sums.txt
pass=0; fail=0
verify() { # dir label
  while IFS= read -r -d '' f; do
    if grep -q "$(sha256sum "$f" | cut -d' ' -f1)" sums.txt; then pass=$((pass+1)); else fail=$((fail+1)); echo "  FAIL [$2] $f"; fi
  done < <(find "$1" -type f ! -name 'fill*' -print0)
}
populate_fat() { # img
  mmd -i "$1" ::/Баримт ::/Зургууд ::/Устах
  mcopy -i "$1" "src/Тайлан 2026.docx" src/албан_бичиг.pdf ::/Баримт/
  mcopy -i "$1" src/photo1.jpg src/зураг_2.jpg ::/Зургууд/
  mcopy -i "$1" src/song.mp3 src/тэмдэглэл.txt ::/
  mcopy -i "$1" src/scan.bmp src/clip.mp4 ::/Устах/
  mdel -i "$1" "::/Баримт/Тайлан 2026.docx" ::/Баримт/албан_бичиг.pdf ::/Зургууд/зураг_2.jpg ::/song.mp3 ::/тэмдэглэл.txt
  mdeltree -i "$1" ::/Устах
}
echo "FAT32 / FAT16"
mkfs.fat -F 32 -C fat32.img 66000 >/dev/null; populate_fat fat32.img
mkfs.fat -F 16 -C fat16.img 32000 >/dev/null; populate_fat fat16.img
for i in fat32 fat16; do "$BIN" recover $i.img --out out_$i >/dev/null 2>&1; verify out_$i $i; done

echo "Formatted FAT32 (deep scan)"
cp fat32.img fmt.img; mkfs.fat -F 32 fmt.img >/dev/null
"$BIN" recover fmt.img --out out_fmt --deep >/dev/null 2>&1; verify out_fmt formatted

echo "NTFS (fragmented)"
truncate -s 32M ntfs.img; mkntfs -F -Q -q ntfs.img 2>/dev/null; ntfs-3g ntfs.img mnt
cp "src/Тайлан 2026.docx" src/зураг_2.jpg src/тэмдэглэл.txt mnt/
for i in $(seq 1 40); do head -c 100000 /dev/urandom > mnt/fill$i; done
head -c 100000000 /dev/zero > mnt/big 2>/dev/null || true; sync
for i in $(seq 1 2 40); do rm mnt/fill$i; done; sync
head -c 1200000 /dev/urandom > mnt/frag.bin; sha256sum mnt/frag.bin >> sums.txt; sync
rm mnt/frag.bin "mnt/Тайлан 2026.docx" mnt/зураг_2.jpg mnt/тэмдэглэл.txt; sync; umount mnt
"$BIN" recover ntfs.img --out out_ntfs >/dev/null 2>&1; verify out_ntfs ntfs

echo "exFAT"
truncate -s 64M exfat.img; mkfs.exfat exfat.img >/dev/null; L=$(losetup -f --show exfat.img); mount.exfat-fuse "$L" mnt
cp src/photo1.jpg src/албан_бичиг.pdf src/voice.wav mnt/; sync; rm mnt/photo1.jpg mnt/албан_бичиг.pdf mnt/voice.wav; sync; umount mnt; losetup -d "$L"
"$BIN" recover exfat.img --out out_exfat >/dev/null 2>&1; verify out_exfat exfat

echo
echo "Passed: $pass   Failed: $fail"
[ "$fail" -eq 0 ]
