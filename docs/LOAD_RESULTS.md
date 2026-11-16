# Snapshot ve cache yükleme karşılaştırması — 16 Kasım 2026

0.4.0, aynı D: NTFS diski ve aynı frozen 12.285.803 kayıt üzerinde CLI başlangıcını **3,073 → 1,598 saniye** medyana indirdi (**%48,0**). Private commit yaklaşık **1,012 GiB** kaldı; bu turda anlamlı RAM azalması ölçülmedi. Önceki [0.3 raporundaki](CACHE_RESULTS.md) 3,051 saniye tarihsel sonuçtur; aşağıdaki karşılaştırmada korunmuş 0.3 executable yeniden ölçüldü.

## Uygulanan değişiklik

- Snapshot kayıtları (48 byte), özgün UTF-16 adlar, cache grupları (40 byte) ve u32 tabloları 256 KiB'lık yeniden kullanılan blokla çözülür. Her alan için küçük okuma çağrıları kaldırıldı.
- Snapshot ve cache payload checksum'u okunan byte'lar üzerinde ayrıştırmayla birlikte hesaplanır. Ayrı tam checksum okuması ve başa dönme kaldırıldı. Tüm payload için ikinci büyük buffer veya mmap kullanılmaz.
- Snapshot ID sırası, flag ve arena aralıkları ayrıştırma sırasında doğrulanır; aynı kayıtları yeniden dolaşan son kontrol geçişi kaldırıldı. Cache'in üyelik, maske, tekillik, sıralama ve kapsam kontrolleri korunur. CRC, uzunluk ve EOF denetlenmeden sonuç dışarı verilmez.
- Snapshot/cache wire formatı sürüm 1, delta protokolü ve arama anlamı değişmedi. Ölçülen cache'ler önceki sürümden kaldı; yeniden oluşturulmadı. Delta replay ve cache'in güncel görünüme bağlanması korunur.

Bu değişiklik kararlı bellekteki kayıtları/tabloları küçültmez. Okuma, decode ve tekrar dolaşma maliyetini azaltır; format uyumluluğunu korur.

## Aynı frozen corpus — delta yok

Makine: Windows 11 Pro build 26300, AMD Ryzen 9 9950X3D, Samsung SSD 990 EVO Plus 4 TB NVMe, yaklaşık 126 GiB görünür RAM. Kaynak `comparison.rsidx`: 908.979.979 byte; mevcut `.search`: 173.439.480 byte. 12.285.803 kayıt her iki sürümde aynıdır.

Her sürüm için 5 yeni süreç, dönüşümlü sıra, child CPU affinity mask 1 (CPU0); öncesinde ölçüm dışı cache-hit. Sıfır eşleşmeli aynı sorgu ve limit 0 kullanıldı. Uçtan uca süre süreç başlatmadan çıkışa kadardır; load/prepare/query ayrı sayaçlardır. Tabloda medyan (min–max) vardır; bileşen medyanlarının toplamı uçtan uca medyanla birebir eşleşmek zorunda değildir.

| Metrik | 0.3.0 | 0.4.0 |
| --- | --- | --- |
| Snapshot yükleme | 1,664 s (1,618–1,709) | 0,323 s (0,311–0,342) |
| Cache okuma/doğrulama ve hazırlık | 1,267 s (1,193–1,435) | 1,185 s (1,164–1,274) |
| Uçtan uca CLI | 3,073 s (2,957–3,176) | 1,598 s (1,576–1,699) |
| Process CPU zamanı | 3,000 s | 1,531 s |
| Private commit | 1.086.386.176 byte | 1.086.345.216 byte |
| Process tepe private commit | 1.086.386.176 byte | 1.086.345.216 byte |
| Working set | 1.089.314.816 byte | 1.089.245.184 byte |
| Süreç read transfer | 2.164.838.798 byte | 1.082.419.459 byte |

Snapshot yükleme **%80,6**, hazırlık **%6,4**, CPU **%49,0** azaldı. Private commit farkı yalnız 40 KiB; anlamlı RAM kazancı değildir. Büyük kalan maliyet cache'in anlamsal doğrulamasıdır; bu kontroller kaldırılmadı.

Kaynak ve cache SHA256'ları tüm örnekler boyunca sabit kaldı; bütün yeni süreçler cache-hit, build/write süresi sıfırdır. OS/file cache boşaltılmadı; bu **yeni süreç başlangıcı** ölçümüdür, cold-SSD ölçümü değildir. Read transfer, Windows süreç sayacıdır; fiziksel disk trafiği değildir. 5 örnekten p95/p99 iddiası yapılmaz.

## Aynı base + iki committed delta

Önceki native kabulden korunmuş ayrı corpus: 12.274.534 güncel kayıt, delta sırası 2, 22 canlı overlay kaydı, 7 gölgelenmiş base satırı. Aynı protokolle 5'er yeni süreç kullanıldı. Her iki binary aynı base, delta ve cache byte'larını okudu; üç dosyanın hash'i ile sıra/kayıt sayısı sabit kaldı. Bu tablo yukarıdaki delta içermeyen corpus ile doğrudan hız kıyaslaması değildir.

| Metrik | 0.3.0 | 0.4.0 |
| --- | --- | --- |
| Source + delta yükleme | 1,618 s (1,595–1,642) | 0,274 s (0,269–0,328) |
| Cache hazırlık ve delta bağlama | 1,116 s (1,099–1,277) | 1,059 s (0,985–1,118) |
| Uçtan uca CLI | 2,873 s (2,807–3,035) | 1,426 s (1,342–1,544) |
| Private commit | 1.097.949.184 byte | 1.097.908.224 byte |
| Process tepe private commit | 1.097.973.760 byte | 1.097.969.664 byte |
| Process CPU zamanı | 2,734 s | 1,359 s |
| Süreç read transfer | 2.161.665.296 byte | 1.080.836.060 byte |

Delta dahil toplam başlangıç **%50,4** azaldı. Cache-hit ve bound sequence 2 korundu. Cache hazırlığı delta'nın tamamını tekrar indekslemez; mevcut overlay güncel source'a bağlanır.

## Doğruluk ve gerçek disk kabulü

Yeni native D: kabulü 0.4 binary ile **12.277.260 başlangıç kaydı** oluşturdu. Bu aktif disk koşusu, frozen performans corpus'larından ayrıdır.

- İlk ayrı sorgu cache-hit; iki ardışık refresh sonrası cache-hit ve bound sequence 1/2.
- Yeniden adlandırma, taşıma, resize, create/delete, Unicode ad ve sıfır boyut bağımsız `Get-ChildItem` sonuçlarıyla eşleşti.
- Base ve search-cache SHA256'ları iki delta boyunca değişmedi. Delta log 7131 byte, bekleyen metadata yenilemesi 0; farklı süreçlerde checkpoint/replay doğru.
- Delta içermeyen ve delta sırası 2 olan corpus üzerinde dokuz sıcak sorgu grubunun tam sayımı ve dönen ID sırası doğrusal referansla eşleşti (`--verify-linear`, 100 ölçüm + 5 warmup/grup). Bu koşular başlangıç tablosuna katılmadı.
- **33 test geçti**, başarısız/ignored yok; doc-test 0 bu sayıya dahil değil. Yeni testler kayıt/UTF-16/cache blok sınırlarını, tam 64-bit kimlikleri, signed USN'yi, bozuk UTF-16'nın korunmasını, geçerli CRC ile bozuk kayıt alanlarını, kısa okumaları, checksum/trailing/truncation reddini kapsar. Önceki cache/delta bozulma ve overlay testleri de geçti.
- `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo test --workspace --locked`, `cargo build --release --bins --examples --locked` geçti.

Fixture doğrulaması bütün diskte her kaydın ayrı ayrı eksiksizliğini kanıtlamaz. Fiziksel güç kesintisi, hosted CI, uzun süreli servis ve başka donanım denenmedi. GUI ve disk alanı haritası bu turun kapsamı değildir.

## Yeniden çalıştırma ve kanıtlar

```powershell
.\scripts\benchmark-startup.ps1 `
  -Index .temp_files\optimization-20261116\comparison.rsidx `
  -BaselineExe .temp_files\load-20261116\baseline.exe `
  -RunDirectory D:\Codebox\RustSearch\.temp_files\load-new-run `
  -Trials 5

# Aynı komut, delta içeren korunmuş source ile de çalıştırılabilir.
# Gerçek disk kabulü için yönetici PowerShell:
.\scripts\ntfs-acceptance.ps1 -Volume D:
```

[Başlangıç örnekleri ve özet](../.temp_files/load-20261116/startup-summary.json), [delta içeren başlangıç karşılaştırması](../.temp_files/load-20261116/delta/startup-summary.json), [sıcak sorgu oracle kontrolü](../.temp_files/load-20261116/hot.json), [delta oracle kontrolü](../.temp_files/load-20261116/delta/hot.json), [yeni NTFS kabulü](../.temp_files/ntfs-load-1794864510790/acceptance.json), [makine/binary bilgisi](../.temp_files/ntfs-load-1794864510790/machine.json).

Eski 0.3 binary SHA256: `D31F61409CE23E13A10CBA973D0BFE2F518A89D9A8076E9DEB8FE954B05D4841`.

Doğrulanan 0.4 binary SHA256: `E47DF130DCFF8B583FB9D02489E73F363475FCAC3781025DE9F8FFB23C3986F7`.

[Kaynak manifesti](../.temp_files/load-20261116/source-manifest.json) ve [artefakt hash'leri](../.temp_files/load-20261116/artifact-hashes.json) yerel kanıt klasöründedir. `.temp_files` metadata içerir ve otomatik yayımlanmaz.
