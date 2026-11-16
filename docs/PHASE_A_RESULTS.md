# A aşaması — Gerçek disk fizibilite sonucu

16 Kasım 2026. Durum: çalışan Windows CLI prototipi, gerçek NTFS biriminde kabul testi geçti. Bu sonuç, Everything'den daha hızlı bir ürün elde edildiği anlamına gelmez.

## Test edilen ortam ve kapsam

- Windows 11 Pro, build 26300; AMD Ryzen 9 9950X3D; yaklaşık 126 GiB işletim sistemine görünür RAM.
- D: — Samsung SSD 990 EVO Plus 4TB, NVMe, NTFS.
- Rust/Cargo 1.93.0; release derlemesi, MSVC, kilitli Cargo bağımlılıkları.
- İndeksleme sonunda 12.285.944 canonical dosya/klasör kaydı; ayrı journal refresh sonrasında 12.285.803 kayıt. Disk aktifti; sayı farkı beklenen canlı değişikliklerden kaynaklanır.
- Native backend bir dosya referansına bir canonical ad tutuyor. Bütün hard-link adlarının kapsamı henüz yok.
- Boyut toplama 5000 uygun dosya denemesiyle sınırlandı; 4999'u başarılıydı. Journal refresh sonrasında boyutu bilinen dosya sayısı 5499 oldu. Diskin tamamının boyutu indekslenmedi.

Kabul ve benchmark binary SHA256:

```text
3774F647E8663C76CB069E7433F147BC2E7CFABE659A723C739F9CF2E518D33A
```

Kaynak dosyalarının SHA256 listesi run klasöründeki `source-manifest.json` içindedir. Başlangıçta Git deposu yoktu; commit oluşturulmadı.

## İndeksleme ve kaynaklar

| Ölçüm | Gözlenen |
| --- | --- |
| Native ad enumerasyonu + sıralama/doğrulama | 25,686 saniye |
| 5000 boyut denemesi | 116,445 ms; 1 başarısız |
| Başlangıç journal replay + yeniden paketleme | 3,912 saniye |
| İndeks komutunun toplam wall süresi | 31,868 saniye |
| İndeks komutunun CPU zamanı | 30,563 saniye; yaklaşık bir çekirdek |
| Başlangıç replay sırasında journal olayları | 14.561 |
| Normal private commit, indeks oluşturma sonunda | 1,287 GiB |
| İndeks oluşturma tepe private commit | 3,073 GiB |
| İndeks oluşturma tepe working set | 2,573 GiB |
| Yeniden yüklenmiş indekste private commit | 1,286 GiB |
| Yeniden yüklenmiş indekste working set | 1,289 GiB |
| Son indeks arena payload'ı | 1.377.352.612 byte; yaklaşık 112 byte/kayıt |
| Ayrı refresh komutunun toplam süresi | 8,831 saniye |
| Refresh tepe private commit | 2,572 GiB |
| Snapshot yükleme, checksum ve doğrulama | 2,735 saniye |

Refresh sırasında iki neslin bellekte tutulması ve bütün snapshot'ın yeniden yazılması hâlâ yüksek bir tepe maliyeti oluşturuyor. CLI her ayrı `search` komutunda snapshot yüklediği için kullanıcıya görünen tek komut süresi yalnız aşağıdaki motor sorgu süresi değildir.

10 saniyelik **yüklenmiş ama işlem yapmayan CLI** deneyinde process CPU artışı 0 ms ve transfer sayacı artışı 0 byte ölçüldü. Bu kısa, kaba çözünürlüklü deney; bir USN daemon'unun veya 10 dakikalık nihai boşta tüketim hedefinin doğrulaması değildir.

I/O değerleri process transfer sayaçlarıdır; metadata IOCTL trafiği `other_transfer_bytes` altında raporlanır. Fiziksel disk trafiği, cache soğukluğu ve enerji tüketimi ölçülmedi.

## Sıcak arama gecikmeleri

Her grupta 5 warm-up ardından 100 örnek. Ölçüm tam eşleşme sayımı + en çok 100 sonuç seçimini içerir; path çözümleme, snapshot yükleme ve UI içermez. OS cache kontrol edilmedi. 100 örnekli p95 ön bulgudur; p99 için örnek sayısı yetersiz olduğundan raporlanmadı.

| Sorgu grubu | p50 | p95 |
| --- | --- | --- |
| Nadir substring | 148,675 ms | 161,547 ms |
| Yaygın substring | 174,958 ms | 184,508 ms |
| Eşleşmeyen substring | 85,255 ms | 98,486 ms |
| Prefix | 48,406 ms | 57,436 ms |
| Tam dosya adı | 51,903 ms | 64,534 ms |
| Uzantı | 61,591 ms | 68,271 ms |
| Büyük dosya filtresi | 20,060 ms | 27,228 ms |
| Largest top-100 | 22,704 ms | 29,888 ms |
| Bütün kayıtlar | 41,559 ms | 50,303 ms |

Büyük dosya ve largest sonuçları **yalnız boyutu bilinen 5499 dosyada** anlamlıdır; filtre sırasında bütün kayıtlar dolaşılmıştır. Bunlar diskteki bütün büyük dosyaları bulma performansı olarak sunulamaz.

Doğrusal tarama bu ölçekte belirgin darboğazdır. 1 milyon kayıt için planlanan p95 ≤10 ms hedefi ayrı referans fixture'da denenmedi; 12,3 milyonluk sonuç o hedefe doğrudan eşitlenmez. Everything için eşdeğer kapsamlı sorgu benchmark'ı yapılmadı; üstünlük iddiası yoktur.

## Doğrulama

- `cargo fmt --all --check` — geçti.
- `cargo clippy --workspace --all-targets --locked -- -D warnings` — geçti.
- `cargo test --workspace --locked` — **18 test geçti**, başarısız/ignored test yok. Doc-test blokları 0 test içeriyor; bu sayı test kapsamına eklenmedi.
- `cargo build --release --locked` — geçti.
- Native gerçek disk kabul script'i — geçti. Başlangıçta 3 fixture dosyası, ardından rename, move, resize, create, delete, Unicode ve sıfır boyut kontrolleri bağımsız klasör enumerasyonuyla doğrulandı.
- Son replay'de bekleyen metadata yenilemesi: 0; refresh hata dağılımı boş.
- Gerçek junction döngüsünde normal klasör taraması hedefi izlemedi.
- Snapshot'ın DACL kalıtımı kapalı ve mevcut kullanıcı/SYSTEM/Administrators izinleri incelemeyle doğrulandı.
- Checksum hasarı, kesilmiş/uydurulmuş snapshot uzunlukları, hatalı boyut argümanı, olmayan indeks ve raporun snapshot üzerine yazılmasını reddetme yolları test edildi.

Fixture, bütün diskteki 12 milyon kaydın her birinin bağımsız oracle ile doğrulandığı anlamına gelmez. Gerçek journal silme/sarma, fiziksel disk çıkarma, uyku/uyanma ve süreç keserek çökme kabulü çalıştırılmadı. Servis, GUI, FAT/ReFS/NAS, allocation/ADS boyutları ve bütün hard-link adları henüz yoktur.

## Bu turda ölçümle bulunan ve düzeltilen iki sorun

1. Dosyanın güncel USN kaydını okuyan isteğin hizalaması: Windows 4 byte hizalama beklerken Rust yapısının doğal hizalaması 2 idi. Gerçek dosya regresyon testi önce OS error 1784 ile başarısız oldu; hizalanmış istek ve output buffer ile geçti. Tam birim kabul testi de aynı rename/move/boyut kontrolleriyle tekrar geçti.
2. Snapshot yeniden paketlemede kapasite: eski uzunluğa reserve yapıp bir yeni kayıt eklemek, büyük arenaları iki katına çıkarıyordu. Küçük regresyon önce 75.075 byte payload için 150.000 byte tahsisle başarısız oldu; değişiklikler dahil ön hesaplamayla geçti. Yaklaşık 12,3 milyon kayıtlık gerçek koşulda arena tahsisi 2,56 GiB'den 1,28 GiB'ye indi. Karşılaştırma iki canlı disk gözlemi arasındadır; kayıt sayıları küçük miktarda farklıdır. Bu bellek sonucu indeksleme hızlanması iddiası değildir.

## Sonraki teknik öncelikler

1. Tam ad/prefix için kompakt sıralı ad indeksi; substring için seçici aday daraltma. Bellek bütçesini koruyarak mevcut doğrusal tarama baseline'ına karşı karşılaştırma.
2. Değişiklik katmanı ve aşamalı snapshot yazımı: her refresh'te 1,28 GiB yeniden paketleme/yazmayı ve iki nesil tepesini kaldırma.
3. Boyut toplamayı tüm dosyalara genişletme; hard-link ad keşfi, allocation/sparse/compression/ADS doğruluğu. Alan haritası bundan sonra.
4. 1 milyon kayıt referans profili, en az 1000 sorgu örneği, kontrol edilmiş başlangıç deneyleri ve Everything ile aynı kapsamlı karşılaştırma.

## Yerel kanıtlar ve çalıştırma

Son doğrulanmış koşu: [run klasörü](../.temp_files/ntfs-verified-1794855229047/).

- [Native indeksleme raporu](../.temp_files/ntfs-verified-1794855229047/index.json)
- [Journal refresh raporu](../.temp_files/ntfs-verified-1794855229047/refresh.json)
- [Kabul sonucu](../.temp_files/ntfs-verified-1794855229047/acceptance.json)
- [Sıcak arama raporu](../.temp_files/ntfs-verified-1794855229047/bench.json)
- [Boşta CLI ölçümü](../.temp_files/ntfs-verified-1794855229047/idle.json)

```powershell
# Son snapshot üzerinde arama; yönetici yetkisi gerekmez.
.\target\release\rustsearch.exe search --index .temp_files\ntfs-verified-1794855229047\native.rsidx --query RustSearch --limit 20

# Tam kabulü yeni izole fixture ile yeniden çalıştırma: yönetici PowerShell.
.\scripts\ntfs-acceptance.ps1 -Volume D:
```

İlk başarısız ve ara koşuların JSON/log kanıtları `.temp_files/ntfs-1794854602545` ve `.temp_files/ntfs-final-1794854966615` altında kaldı. Yer kaplayan eski tam birim snapshot'ları kaldırıldı; son doğrulanmış snapshot korundu. `.temp_files` ve indeks dosyaları `.gitignore` kapsamındadır; yerel rapor bağlantıları depoya taşınacak kanıt yerine geçmez.
