# Cache doğrulama ve başlangıç — 17 Kasım 2026

İlk public proje sürümü **0.0.1** olarak kabul edilir. Önceki raporlardaki 0.2/0.3/0.4 sayıları yayımlanmamış deney binary etiketleridir. Bu turda önceki blok yükleyici yeniden ölçüldü; eski 1,185 saniyelik cache / 1,598 saniyelik toplam değerlerle farklı koşular doğrudan katsayıya çevrilmedi.

Aynı D: corpus'unda cache hazırlık medyanı **1,200 → 0,776 saniye (%35,4 azalma)**; uçtan uca yeni süreç başlangıcı **1,708 → 1,187 saniye (%30,5 azalma)**. Kararlı RAM yaklaşık **1,012 GiB** kaldı; anlamlı bellek azalması ölçülmedi.

## Ne değişti?

Önceki cache, grupların posting listelerini saklıyordu. Üyelik denetimi büyük base kayıt tablosuna grup sırasıyla dağınık erişiyordu. Cache **v2**, aynı boyuttaki u32 ilişki tablosunu **base satırı → grup** düzeninde saklar. Yükleyici source kayıtlarını sıralı okurken üyeliği doğrular ve bellekte aynı sıralı posting listelerini kurar. Grup alanları geçici imleç olarak kullanılır; normal yüklemede ayrı satır/grup imleç tablosu tahsis edilmez.

- Grup anahtar/tür sırası, aralıklar, temsilci minimum ID, karakter/trigram maskeleri, bütün source satırlarının üyeliği ve tam grup kapsamı doğrulanır.
- V2 grup aralıkları canonical prefix toplamları olmalıdır. Satırlar source ID sırasıyla yerleştirildiği için her posting listesi sıralıdır. Eksik/fazla grup üyeliği imleç bitişlerinde reddedilir.
- Uzantı tekilliği/kapsamı, boyut kapsamı ve sırası, CRC, byte uzunluğu, base nesli ve EOF kontrolleri korunur.
- Karakter maskesinde 256 elemanlı sabit lookup tablosu kullanılır; grup karakter/trigram maskeleri aynı geçişte hesaplanır. Uzantı ayrımı mevcut `memchr` bağımlılığıyla yapılır. Yeni bağımlılık veya unsafe kod eklenmedi.
- Snapshot ve delta formatı değişmez. Immutable base cache'e committed delta overlay'i ayrıca bağlanır; delta geldiğinde cache yeniden yazılmaz.

## Tek seferlik eski cache dönüşümü

V1 cache bütün eski doğrulamalardan geçtikten sonra kilit alınabiliyorsa atomik olarak v2'ye çevrilir: `cache.state=upgraded`, `build_ms=0`, dönüşüm `write_ms` içindedir. Kilit alınamazsa veya yazılamazsa doğrulanmış eski tabloyla arama devam eder (`legacy_hit`). Eksik/bozuk cache için yeniden kurma yolu sürer. Eski deney CLI'leri v2'yi okuyamaz.

Dönüşüm sırasında bir kez satır başına u32 geçici owner tablosu gerekir: ana corpus'ta **49.143.212 byte (46,9 MiB)** ek payload. Bu geçici tahsis normal v2 cache-hit yüklemesinde yoktur. `index`/`compact` cache yayını da bu writer'ı kullanır; ilk kurulum/yayın tepe belleği ile normal sorgu başlangıcı karıştırılmamalıdır. Delta corpus'unun final binary ile dönüşümü **232,6 ms** yazma/dönüştürme, yaklaşık **1,164 s** v1 okuma/doğrulama sürdü; bu tek koşudur. Migration ve yeniden kurma maliyetleri aşağıdaki hit tablosuna dahil değildir.

## Aynı disk ve byte-identical source karşılaştırması

Windows 11 Pro build 26300, Ryzen 9 9950X3D, Samsung SSD 990 EVO Plus 4 TB NVMe, yaklaşık 126 GiB görünür RAM. Frozen corpus **12.285.803 kayıt**, source 908.979.979 byte. V1 ve v2 cache boyutu aynı: **173.439.480 byte**.

Her varyant 5 yeni süreçte, dönüşümlü sıra ve CPU affinity mask 1 ile çalıştı. Aynı sıfır eşleşmeli sorgu, limit 0 kullanıldı. Varyantlara ayrı cache yolları verildi; source dosyası NTFS hard link ile aynı byte'ları paylaşır. Source ve varsa delta SHA256'larının eşitliği karşılaştırma öncesi ve sonrası doğrulandı. Her varyantın kendi cache SHA256'sı sabit kaldı; bütün örnekler hit, build/write sıfırdır.

| Metrik | Önceki blok yükleyici (deney 0.4) | Güncel 0.0.1 |
| --- | --- | --- |
| Snapshot + varsa delta yükleme | 0,314 s (0,294–0,351) | 0,308 s (0,278–0,394) |
| Cache hazırlığı | 1,200 s (1,181–1,304) | 0,776 s (0,691–0,927) |
| Uçtan uca CLI | 1,708 s (1,629–1,905) | 1,187 s (1,092–1,478) |
| Process CPU zamanı | 1,500 s (1,453–1,609) | 1,094 s (0,984–1,312) |
| Private commit | 1.086.324.736 byte | 1.086.308.352 byte |
| Process tepe private commit | 1.086.324.736 byte | 1.086.308.352 byte |
| Working set | 1.089.224.704 byte | 1.089.220.608 byte |

Read transfer her iki varyantta **1.082.419.459 byte**: bu tur okunan byte sayısını düşürmez; doğrulamanın CPU/bellek erişim düzenini iyileştirir. Private commit medyan farkı yalnız **16 KiB**; RAM tasarrufu iddiası yapılmaz.

OS/file cache boşaltılmadı. Sonuçlar cold-SSD ölçümü değildir. Süreç transfer sayaçları fiziksel disk I/O'su değildir. Tablolar medyan (min–max); 5 örnekte p95/p99 iddiası yoktur. Bileşen medyanları toplamı uçtan uca medyanla birebir eşleşmek zorunda değildir. Sıcaklık, frekans ve makine yükü süreleri etkiler; eski binary'nin bu turdaki yeniden ölçümü kontroldür.

## Delta sırası 2 olan ayrı corpus

Aynı önceki native source: **12.274.534 güncel kayıt**, iki committed delta, 22 canlı overlay, 7 gölgelenmiş base satırı. V1/v2 cache boyutu **173.082.616 byte**. Yine 5'er yeni süreç ve aynı protokol. Her iki varyantta base/delta byte'ları, sıra ve kayıt sayıları aynıdır; dosyalar değişmedi.

| Metrik | Önceki blok yükleyici (deney 0.4) | Güncel 0.0.1 |
| --- | --- | --- |
| Snapshot + varsa delta yükleme | 0,352 s (0,306–0,364) | 0,353 s (0,294–0,363) |
| Cache hazırlığı | 1,155 s (1,132–1,277) | 0,872 s (0,783–0,900) |
| Uçtan uca CLI | 1,683 s (1,544–1,742) | 1,375 s (1,186–1,434) |
| Process CPU zamanı | 1,469 s (1,422–1,641) | 1,203 s (1,078–1,234) |
| Private commit | 1.097.887.744 byte | 1.097.875.456 byte |
| Process tepe private commit | 1.097.924.608 byte | 1.097.904.128 byte |
| Working set | 1.099.288.576 byte | 1.099.284.480 byte |

Cache hazırlığı **%24,5**, toplam başlangıç **%18,3** azaldı. RAM yine yaklaşık aynı kaldı. Bu ayrı corpus'un süreleri üstteki corpus ile doğrudan karşılaştırılmaz.

## Doğrulama

- `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo test --workspace --locked`, `cargo build --release --bins --examples --locked`.
- **36 test geçti**, başarısız/ignored yok; 0 doc-test bu sayıya eklenmedi. V1 dönüşümü, kilitli migration'da eski cache kullanımı, committed delta bağlama, v2 roundtrip ve boş source kapsanır.
- Geçerli CRC ile yanlış grup, minimum ID, key aralığı, maskeler, noncanonical range ve toplamı aynı tutulan uydurma grup sayıları reddedilir. Önceki truncation/checksum/uzantı tekilliği, farklı base, cache yazma hatası ve iki delta nesli testleri korunur.
- İki frozen corpus üzerinde dokuzar sıcak sorgu grubu, tam sayım ve dönen ID sırasıyla doğrusal oracle'a karşı kontrol edilir (100 ölçüm + 5 warmup/grup). Bu koşular başlangıç tablosuna dahil değildir.
- Gerçek D: NTFS kabulü **12.280.655 başlangıç kaydıyla geçti**. Rename, move, resize, create/delete, Unicode ve sıfır boyut bağımsız dizin enumerasyonuyla eşleşti. İlk sorgu ve iki delta nesli cache-hit; bağlanan sıra 0/1/2. Base ve cache SHA256'ları sabit kaldı, delta log 7593 byte ve bekleyen metadata yenilemesi 0.

Hosted CI, fiziksel güç kesintisi, uzun süreli servis ve başka donanım bu turda denenmedi. GUI/disk haritası henüz yoktur. İlk public depo kaynak kodunu içerir; ham disk indeksleri ve metadata raporları yüklenmez.

## Yeniden ölçme

```powershell
# Byte-identical source, cache'ler ayrı yollarda.
.\scripts\benchmark-startup.ps1 `
  -Index .temp_files\optimization-20261116\comparison.rsidx `
  -CurrentIndex .temp_files\validation-20261117\current.rsidx `
  -BaselineExe .temp_files\validation-20261117\baseline.exe `
  -RunDirectory D:\Codebox\RustSearch\.temp_files\validation-new-run `
  -Trials 5

# Yönetici PowerShell:
.\scripts\ntfs-acceptance.ps1 -Volume D:
```

Yerel kanıtlar `.temp_files` altındadır ve public depoda bulunmaz: [başlangıç özeti](../.temp_files/validation-20261117/startup/startup-summary.json), [delta başlangıcı](../.temp_files/validation-20261117/delta-startup/startup-summary.json), [v1 dönüşümü](../.temp_files/validation-20261117/delta-startup/cache-warmup.json), [sıcak oracle](../.temp_files/validation-20261117/hot.json), [delta oracle](../.temp_files/validation-20261117/delta-hot.json), [native kabul](../.temp_files/ntfs-validation-20261117/acceptance.json).

Önceki binary SHA256: `E47DF130DCFF8B583FB9D02489E73F363475FCAC3781025DE9F8FFB23C3986F7`.

Doğrulanan 0.0.1 SHA256: `C44E3B67D96299B47D9396997D425C02274319D031C504B8F09A02DE492ED344`.
