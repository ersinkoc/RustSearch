# RustSearch

Windows için düşük kaynak tüketimli dosya arama ve disk alanı analiz uygulaması.

**Sürüm: 0.0.1.** Windows CLI fizibilite prototipi. NTFS/USN indeksleme, snapshot, ad araması, boyut filtreleri, journal replay ve kaynak ölçümü uygulanmıştır. Masaüstü arayüzü henüz yoktur.

İlk arama optimizasyonunda sıralı ad grupları, substring aday özetleri, paylaşılan ad havuzları ve ana snapshot'ı değiştirmeyen delta güncellemeleri eklendi. Aynı 12,29 milyon kayıtta sıcak arama ve bellek karşılaştırması [optimizasyon raporunda](docs/OPTIMIZATION_RESULTS.md).

Kalıcı cache denemesinde bu arama yardımcıları `.rsidx.search` cache'inde yeniden kullanılır. `index` ve `compact` cache'i bir kez hazırlar; delta'lar cache'i yeniden yazmadan küçük bir görünüm katmanına bağlanır. Aynı veriyle süreç başlangıcı medyanı 6,06 → 3,05 saniye ölçüldü: [başlangıç/cache raporu](docs/CACHE_RESULTS.md).

Blok okuma optimizasyonunda snapshot ve cache bloklar halinde okunur; checksum ayrıştırmayla aynı geçişte hesaplanır. Doğruluk denetimleri korunarak aynı diskte 12,29 milyon kaydın CLI başlangıcı medyanı 3,07 → 1,60 saniyeye indi. RAM yaklaşık aynı kaldı: [yükleme karşılaştırması](docs/LOAD_RESULTS.md).

Ölçüm belgelerindeki eski 0.2/0.3/0.4 numaraları yayımlanmamış deney binary etiketleridir. Projenin ilk sürümü 0.0.1 olarak kabul edilir. Ham disk indeksleri ve yerel kanıt dosyaları GitHub deposuna dahil edilmez.

Güncel cache düzeninde aynı verinin hazırlığı 1,20 → 0,78 saniye, toplam CLI başlangıcı 1,71 → 1,19 saniye ölçüldü; RAM yaklaşık aynı kaldı: [cache doğrulama raporu](docs/CACHE_VALIDATION_RESULTS.md).

## Ürün fikri

Tek bir güncel dosya indeksinden üç soruya cevap ver:

- Dosyam nerede? Ad, yol, uzantı, boyut ve tarihle anlık arama.
- Alanı ne tüketiyor? Klasör ağacı, boyut sıralaması ve etkileşimli disk haritası.
- En büyük dosyalar hangileri? Tüm disklerde veya seçilen klasörde büyük dosya listesi.

İlk sürüm dosya içeriklerini indekslemez. Bu kapsam kullanıcı tarafından 16 Kasım 2026 tarihinde seçilmiştir.

## Temel yaklaşım

- Rust çekirdek, Windows API entegrasyonu ve yerel masaüstü arayüzü.
- Önce yerel NTFS diskleri; başlangıç için Windows 11 x64.
- İsim indeksi için toplu metadata okuma, güncellemeler için USN Journal.
- Arama, klasör toplamları ve disk haritası için ortak veri modeli.
- Sınırlı iş kuyrukları, iptal edilebilir sorgular, olay tabanlı güncelleme.
- Dosya içeriklerini okumadan metadata üzerinden çalışma.
- Yerel veri; hesap, bulut veya telemetri gerektirmeyen kullanım.

## Hız iddiası

Everything zaten hızlı indeksleme ve arama sunuyor; klasör boyutu indeksleme desteği de var. Rust kullanmak veya MFT okumak tek başına üstünlük kanıtı değildir. Hedefimiz, eşdeğer kapsam ve doğrulukta daha düşük kaynak tüketimiyle özellikle büyük indekslerde ve aramadan disk analizine geçişte ölçülebilir avantaj üretmek.

“Everything'den daha hızlı” ifadesi ancak sürümü ve ayarları kaydedilmiş karşılaştırmalarla kullanılacak. Hedefler, henüz ölçülmüş sonuç değildir.

## Belgeler

- [Ürün, mimari ve aşamalı geliştirme planı](docs/PROJECT_PLAN.md)
- [Performans hedefleri ve karşılaştırma protokolü](docs/BENCHMARKS.md)
- [CLI derleme, kullanım ve kapsam sınırları](docs/CLI.md)
- [Gerçek NTFS diskinde A aşaması sonuçları](docs/PHASE_A_RESULTS.md)
- [Arama, bellek ve delta güncellemesi karşılaştırması](docs/OPTIMIZATION_RESULTS.md)
- [Kalıcı arama cache'i ve başlangıç karşılaştırması](docs/CACHE_RESULTS.md)
- [Snapshot/cache yükleme ve delta doğruluğu karşılaştırması](docs/LOAD_RESULTS.md)
- [Cache düzeni, doğrulama ve başlangıç karşılaştırması](docs/CACHE_VALIDATION_RESULTS.md)

## Hızlı başlangıç

Windows, Rust 1.93+ ve MSVC C++ build araçları gerekir. Bağımlılıklar `Cargo.lock` ile sabitlenmiştir.

```powershell
cargo build --release --locked
New-Item -ItemType Directory -Path .temp_files -Force

# Normal kullanıcı: erişilebilir bir klasörün snapshot'ı.
.\target\release\rustsearch.exe index --root .\crates --sizes --output .temp_files\local.rsidx
.\target\release\rustsearch.exe search --index .temp_files\local.rsidx --ext rs

# Yönetici PowerShell: bütün D: biriminin native NTFS ad indeksi.
.\target\release\rustsearch.exe index --volume D: --output .temp_files\native.rsidx
.\target\release\rustsearch.exe bench --index .temp_files\native.rsidx --iterations 1000 --report .temp_files\bench.json
```

Native prototip dosya referansı başına tek canonical ad tutar; ek hard-link adlarını henüz listelemez. `--sizes` kullanılmadığında boyutlar bilinmiyor olarak kalır. Snapshot canlı değildir: native indeks `refresh` ile güncellenir, klasör taraması yeniden oluşturulur. İçerik araması ve diskte ayrılmış fiziksel alan hesabı yoktur.

## Resmî kaynaklar

- [Everything: indeksler ve klasör boyutları](https://www.voidtools.com/support/everything/indexes/)
- [Microsoft: NTFS metadata enumerasyonu](https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ni-winioctl-fsctl_enum_usn_data)
- [Microsoft: USN değişiklik günlüğü](https://learn.microsoft.com/en-us/windows/win32/fileio/change-journals)

Araştırma tarihi: 16 Kasım 2026. Bu depo Everything veya voidtools ile bağlantılı değildir.
