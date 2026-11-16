# Kalıcı arama cache'i — 0.3.0 prototipi

16 Kasım 2026. Windows CLI üzerinde uygulandı ve aynı D: NTFS diskinde doğrulandı. Yayın/release yapılmadı. [0.2.0 sıcak arama/delta sonuçları](OPTIMIZATION_RESULTS.md) korunmuştur.

## Değişiklik

Önceki süreç başı hazırlık, ad gruplarını yeniden hash'leme, posting oluşturma ve sıralama yapıyordu. Yeni sürüm bunları binary `.rsidx.search` dosyasında tutar. `index` ve `compact` bir kez hazırlar; source önceden varsa ilk sorgu gerektiğinde cache oluşturur. Geçerli cache sonraki CLI süreçlerinde okunup doğrulanır.

Cache immutable base'e aittir; log sırasına ait tam bir canlı indeks kopyası değildir. Delta yüklenince değişmiş base satırları maskelenir ve sayımlar düzeltilir; en güncel kayıtlar küçük overlay'de aranır. Yeni/silinmiş/yeniden adlandırılmış dosyalar ile boyut/tür/uzantı değişimleri, büyük cache'i yeniden yazmadan doğru sonuç verir. Parent/path çözümleme güncel source üzerinde kalır.

Cache algoritma/format sürümü, base header kimliği, uzunluklar ve CRC32 ile bağlanır. Kayıt üyeliği, grup maskeleri, posting sırası, uzantı tekilliği/kapsamı ve boyut sırası/kapsamı da doğrulanır. Bu kontroller nedeniyle cache hazırlığı sıfır değildir. Dosya eksik/bozuk/uyumsuzsa yeniden kurulur; yazılamıyorsa arama geçerli source üzerinde bellekte sürer. Kaynak snapshot veya committed delta hataları gizlenmez.

Cache ve lock'ı özel DACL ile oluşturulur. Temp dosya senkronize edilmeden atomik publish yapılmaz. Cache türetilebilir; base+delta source'u korunmalıdır. CRC32 kimlik doğrulama değildir. Gerçek güç kesintisi denenmedi.

## Aynı veri ve aynı çekirdekte başlangıç karşılaştırması

Ortam: Windows 11 Pro build 26300, Ryzen 9 9950X3D, Samsung 990 EVO Plus 4TB NVMe, yaklaşık 126 GiB görünür RAM, Rust 1.93/MSVC release.

**Her iki executable aynı 12.285.803 kayıtlık, 908.979.979 byte frozen source dosyasını okudu.** Kaynak ve cache hash'leri deney boyunca sabitti. 0.2.0 binary korundu; 0.3.0 cache-hit yolu ile karşılaştırıldı.

- Varyant başına 5 ayrı süreç; sıra dönüşümlü.
- İki varyant da aynı mantıksal CPU'da çalıştırıldı (`ProcessorAffinity=1`). Ayar yalnız child süreçlere uygulandı.
- Aynı eşleşmeyen ad sorgusu, `--limit 0 --json`; tam sayım korunur. Dosya yolu sonuç üretme maliyeti bu senaryoda yoktur.
- Uçtan uca süre process başlatma ve çıkışını da kapsar; aşama/CPU/bellek değerleri child CLI raporundan gelir.
- Ön okumalar/warm-up var; OS/file cache temizlenmedi. Bu yeni süreç başlangıcı testidir, cold-SSD testi değildir.
- Örnek sayısı 5; p95/p99 iddiası yok. Aşağıda medyan ve min–max verilmiştir.

| Metrik | 0.2.0 yeniden hazırlama | 0.3.0 cache-hit |
| --- | --- | --- |
| Source yükleme | 1,659 s (1,605–1,689) | 1,629 s (1,612–1,732) |
| Arama yardımcılarını hazırlama | 4,292 s (4,080–4,356) | 1,244 s (1,220–1,314) |
| Uçtan uca CLI | 6,060 s (5,795–6,248) | 3,051 s (2,935–3,100) |
| Process CPU zamanı | 5,875 s | 2,906 s |
| Normal private commit | yaklaşık 1,013 GiB | yaklaşık 1,012 GiB |
| Process tepe private commit | yaklaşık 1,216 GiB | yaklaşık 1,012 GiB |
| Normal working set | yaklaşık 1,015 GiB | yaklaşık 1,015 GiB |

Hazırlık medyanı **%71**, toplam başlangıç medyanı **%49,7**, tepe private commit **%16,8** azaldı. Kararlı RAM yaklaşık aynı kaldı; cache bir bellek tasarrufu yerine öncelikle yeniden kurma ve geçici tahsis maliyetini kaldırır.

Önceki rapordaki **8,368 saniye** hazırlık, affinity sabitlenmemiş ayrı bir koşuydu. Onu yeni sabit-çekirdek ölçümüyle doğrudan katsayıya çevirmedik; yukarıdaki eski binary tekrar ölçümü karşılaştırmanın kontrolüdür. Donanım yükü/frekansı ve çekirdek seçimi süreleri etkiler.

Frozen corpus için ek disk cache'i **173.439.480 byte (yaklaşık 165,4 MiB)**. Process private/working-set değerleri OS standby/file cache'inin tamamını temsil etmez. Cache dosyasını okumak da I/O'dur; bedelsiz bellek gibi sunulmaz.

## Delta ve ilk sorgu — canlı D: kabulü

Yeni gerçek NTFS koşusu **12.274.519 kayıt** oluşturdu; frozen corpus'tan farklı ve aktif disktir. Tek seferlik arama cache'i index komutunda **6,391 saniye** kurulum + **142,948 ms** yazım gerektirdi. Toplam index komutu 62,154 saniye sürdü; başlangıç taraması ve diğer indeks işleri de dahildir. Bu tek koşudan ilk indeksleme hızlanması iddiası yapılmadı.

- İlk ayrı CLI sorgusu: `cache.state=hit`, `build_ms=0`, bağlanan delta sırası 0.
- Rename, move, resize, delete, create, Unicode ad ve sıfır boyut: bağımsız klasör enumerasyonuyla doğru.
- Birinci delta ardından: cache-hit, sıra 1, 16 overlay kaydı, delta bağlama **0,092 ms**; hazırlık toplamı yaklaşık 1,399 s.
- İkinci delta ardından: cache-hit, sıra 2, 22 overlay kaydı, delta bağlama **0,110 ms**; hazırlık toplamı yaklaşık 1,431 s.
- Her iki durumda base ve search-cache SHA256'ları sabit kaldı; delta log toplamı 6704 byte oldu. Bekleyen metadata yenilemesi 0.
- İlk hazırlık normal indeks oluşturma aşamasına taşındı. Cache yoksa/uyumsuzsa bir kez yeniden kurma maliyeti devam eder; ortadan kalkmış gibi raporlanmaz.

Bu yöntem küçük delta'larda bütün arama tablolarını yeniden kurmaz. Overlay büyüdükçe bağlama ve sorgu maliyeti büyür; 64 MiB delta log sınırı ve yeni yola `compact` davranışı değişmedi. Cache okunup ayrıştırıldığı için startup'ta hâlâ doğrulama ve source yükleme maliyeti vardır; bellek eşleme veya resident daemon uygulanmadı.

## Doğrulama

- `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo test --workspace --locked`, `cargo build --release --bins --examples --locked` geçti.
- **28 test geçti**, başarısız/ignored test yok; 0 doc-test sayısı buna eklenmedi.
- Kalıcı roundtrip, iki delta nesli, parent taşıma, silme/yeniden adlandırma, yeni dosya, tür/boyut/uzantı değişimi, Unicode, tamamlanmamış delta kuyruğu; sonuçlar doğrusal sorguyla karşılaştırıldı.
- Bozuk checksum, eksik dosya, truncation, eski format, farklı base/compaction nesli, uydurulmuş boyutlar, yanlış posting/maskeler, cache yazma başarısızlığı ve cache oluşturucu kilidi test edildi.
- Geçerli checksum ile tekrar eden uzantı posting'i için regresyon testi önce cache-hit vererek başarısız oldu. Uzantı tekilliği ve yardımcı tablo kapsamı denetimleri eklendikten sonra yeniden kurma yoluna geçti ve sonuçları doğrulandı.
- Gerçek disk native kabulü ve farklı süreçlerden cache/delta kullanımı geçti. Sıcak dokuz benchmark sorgusu da tam sayım ve ID sırasıyla doğrusal referansa karşı kontrol edildi.
- Hosted CI, gerçek güç kesintisi, uzun süreli servis, başka donanım ve işletim sistemleri denenmedi. GUI yoktur.

## Yeniden çalıştırma ve kanıtlar

```powershell
# Aynı snapshot, korunmuş eski binary ve yeni child süreçlerde karşılaştırma.
.\scripts\benchmark-startup.ps1 `
  -Index .temp_files\optimization-20261116\comparison.rsidx `
  -BaselineExe .temp_files\cache-20261116\baseline.exe `
  -RunDirectory D:\Codebox\RustSearch\.temp_files\cache-new-run `
  -Trials 5

# Cache'i bypass ederek kaynak doğrulaması + yeniden kurma maliyetini görmek.
.\target\release\rustsearch.exe search --index .temp_files\optimization-20261116\comparison.rsidx --query RustSearch --no-cache --json

# Gerçek native kabul, yönetici PowerShell.
.\scripts\ntfs-acceptance.ps1 -Volume D:
```

[Özet ve tüm başlangıç örnekleri](../.temp_files/cache-20261116/startup-summary.json), [sıcak sorgu oracle kontrolü](../.temp_files/cache-20261116/hot-cached.json), [canlı NTFS/cache kabulü](../.temp_files/ntfs-cache-1794862816351/acceptance.json), [ilk sorgu](../.temp_files/ntfs-cache-1794862816351/fixture-before.stdout.json), [iki delta sonrası sorgu](../.temp_files/ntfs-cache-1794862816351/fixture-after-again.stdout.json).

Eski binary: `E43FCA23CBD078E8C0B6CA302CAEF8A98607E9BAC85A40A265DC741B3B8EB4C2`.

Doğrulanan 0.3.0 binary: `D31F61409CE23E13A10CBA973D0BFE2F518A89D9A8076E9DEB8FE954B05D4841`.

Kaynak/binary/source/cache hash manifestleri yerel karşılaştırma klasöründe saklanır. `.search`, `.search.lock` ve temp cache dosyaları `.gitignore` kapsamındadır. Cache yeniden üretilebilir; taşımak zorunlu değildir. Source `.rsidx` ve varsa `.delta` birlikte korunur.
