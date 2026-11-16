# Arama ve delta optimizasyonu — 0.2.0 prototipi

16 Kasım 2026. Aynı D: NTFS/NVMe diskinde çalışan CLI uygulaması. Yerel derlemedir; sürüm yayımlanmadı. [İlk aşama](PHASE_A_RESULTS.md) sonuçları tarihsel olarak korunmuştur.

## Karşılaştırma yöntemi

Donanım değişmedi: Windows 11 Pro build 26300, Ryzen 9 9950X3D, Samsung 990 EVO Plus 4TB, yaklaşık 126 GiB görünür RAM; Rust 1.93.0/MSVC release.

Arama karşılaştırmasında disk yeniden taranıp farklı dosya kümesi kullanılmadı. İlk aşamadan kalan **12.285.803 kayıtlık** snapshot donduruldu; korunan 0.1.0 executable ile aynı dokuz sorgu yeniden ölçüldü. 0.2.0 için yalnız veri yerleşimi sıkıştırıldı. **Her bir kaydın** ID, parent, USN, boyut, flags, özgün UTF-16 ad ve arama anahtarı alanları iki snapshot arasında karşılaştırıldı; tamamı aynıydı.

Her iki ölçüm: 5 warm-up + grup başına 100 örnek, tam sonuç sayımı + en çok 100 sonuç, aynı sıralama sözleşmesi. Yeni motor ayrıca her sorgunun sonuç sayısı ve dönen ID sırasını doğrusal referansla karşılaştırdı. p95 ön ölçümdür; p99 raporlanmadı. Cache ve haricî işletim sistemi yükü izole edilmedi; koşular eşzamanlı yapılmadı.

## Sıcak sorgular

| Grup | Korunan eski sürüm p95 | Yeni sürüm p95 |
| --- | --- | --- |
| Nadir substring | 157,389 ms | 9,068 ms |
| Yaygın tek karakter substring | 156,337 ms | 16,531 ms |
| Eşleşmeyen substring | 78,873 ms | 8,325 ms |
| Prefix | 54,972 ms | 0,0067 ms |
| Tam ad | 55,575 ms | 0,0039 ms |
| Uzantı | 68,228 ms | 0,0220 ms |
| Büyük dosya filtresi | 31,255 ms | 0,0007 ms |
| Largest top-100 | 33,244 ms | 0,0004 ms |
| Bütün kayıtlar | 49,402 ms | 0,0004 ms |

Nadir substring yaklaşık 17,4×, yaygın tek karakter sorgusu yaklaşık 9,5× hızlandı. Çok küçük değerler sıcak cache ve mikrobenchmark ölçümleridir; UI veya ayrı CLI sürecinin toplam gecikmesi değildir. Büyük dosya sorguları, aynı **5499 boyutu bilinen dosya** kapsamındadır; diskin bütün dosya boyutları toplanmadı.

### Yapılan değişiklikler

- Aynı normalize ad ve dosya/klasör türündeki kayıtlar gruplanır; bu corpus'ta 2.903.752 grup vardır. Ad eşleşmesi dosya başına yinelenmez; grup sayısı tam sayımı korur.
- Tam ad ve prefix için sıralı gruplarda binary lookup; uzantı için ayrı grup sıralaması; bilinen boyutlar için boyut sıralaması.
- Substring adayları karakter/trigram maskeleriyle elenir; gerektiğinde gerçek byte eşleşmesi doğrulanır. Bu maskeler yanlış negatif üretmez. ASCII harf/rakam tek karakter üyeliği maskede ayrı bitlerle kesin olarak temsil edilir.
- Grup minimum ID'si ve sıralı posting'ler sayesinde ilk 100 ID için milyonlarca kayıt nesnesine rastgele erişilmez. Büyük sonuç kümesi kesilip sayım eksik gösterilmez.
- Boş sorguda toplam/ilk kayıtlar, largest sorgusunda hazır boyut sırası kullanılır.

Genel substring yolu hâlâ benzersiz ad gruplarının sayısıyla büyür; keyfî sorgularda sabit zaman veya tam bir inverted trigram index iddiası yoktur.

## Bellek ve başlangıç maliyeti

| Ölçüm | Eski | Yeni |
| --- | --- | --- |
| Frozen corpus snapshot | 1,28 GiB | 0,85 GiB |
| Normal private commit, arama yardımcıları dahil | 1,286 GiB | 1,012 GiB |
| Sorgu deneyindeki process tepe private commit | 1,287 GiB | 1,216 GiB |
| Normal working set | 1,289 GiB | 1,014 GiB |

Normal private commit yaklaşık **%21,4** düştü. Yeni arama yardımcılarının **173.703.896 byte** ek maliyeti bu karşılaştırmaya dahildir. Özgün UTF-16 adlar ve normalize arama anahtarları ayrı ayrı paylaştırılır; durum/case/Unicode bilgisi atılmaz. Aynı adın farklı disk konumları kayıt olarak korunur. Kazanç adların tekrar oranına bağlıdır; bu tek corpus'tan her veri kümesi için RAM garantisi çıkarılmaz.

**Açık maliyet:** Yeni snapshot yükleme 1,674 saniye, arama yardımcılarını hazırlama **8,368 saniye** sürdü. Eski CLI'de yardımcı hazırlığı yoktu. Dolayısıyla ilk bağımsız CLI sorgusunun uçtan uca açılışı bu turda hızlanmadı; sıcak motor kazanımı bu süreyi içermez. Yardımcılar bu prototipte süreçler arasında cache edilmiyor. Kalıcı oturum veya sürümlü arama cache'i sonraki başlangıç darboğazıdır.

Sıfırdan canlı NTFS indeks oluşturma 35,820 saniye sürdü; ilk aşamadaki 31,868 saniyeden daha yavaştı. Paylaşılan ad havuzlarının hazırlanması ek iştir ve canlı olay kümeleri farklıdır. İndeks oluşturma tepe private commit'i 3,073 GiB'den yaklaşık 2,383 GiB'ye indi. Bu gözlemler kontrollü cold-disk deneyleri değildir.

## Aynı değişiklik kümesinde yazım karşılaştırması

128 **sentetik metadata değişikliği**, aynı dondurulmuş gerçek disk snapshot'ına uygulandı. Gerçek kullanıcı dosyaları değiştirilmedi. Her yol ayrı süreçte çalıştı; kaynak yükleme ve delta yolundaki salt okunur hard-link hazırlığı zamanlamadan çıkarıldı. Eski `Index::apply + save` davranışı mevcut referans implementasyonla, yeni yol `append_delta + apply_delta` ile çalıştırıldı. Çıktı imzaları eşleşti.

| Ölçüm | Tam yeniden paketleme/yazım | Delta |
| --- | --- | --- |
| Güncelleme wall süresi | 5452,068 ms | 4,608 ms |
| Kalıcı yazılan byte | 1.377.352.999 | 22.213 |
| Process tepe private commit | 2,572 GiB | 1,287 GiB |

Bu tek koşuluk kontrollü deneyde zaman yaklaşık 1183×, yazılan veri yaklaşık 62 bin kat azaldı; bunlar bütün gerçek iş yükleri için hız garantisi değildir. CPU sayaçları kısa delta süresinde kaba çözünürlüklüdür. 128 kayıt boyutu ve sonuç imzası her iki raporda aynıydı. Deneyin üretim kodunu kullanan kaynağı: `crates/cli/examples/update_compare.rs`.

Delta artık ana `.rsidx` dosyasını değiştirmez; commit/checkpoint işlemleri `.rsidx.delta` dosyasına eklenir. Okumada sadece güncellenen kayıtlar bellekte eskilerini gölgeler. Birkaç kayıt için ikinci tam indeks nesli kurulmaz. Küçük güncellemelerin ad havuzlarını yeniden tahsis etmemesi için yüklemede 192 KiB boş kapasite ayrılır; daha büyük birikimlerde arena büyümesi hâlâ mümkündür.

## Canlı NTFS kabulü

Yeni tam D: taraması 12.268.945 kayıt üretti. Disk aktifti; frozen arama corpus'undan farklıdır. Fixture'da create/delete/rename/move/resize, Unicode ve sıfır boyut bağımsız klasör enumerasyonuyla doğrulandı.

İlk iki ardışık native refresh:

| Ölçüm | İlk | İkinci |
| --- | --- | --- |
| Journal olayları | 7713 | 6028 |
| Eklenen delta | 190.658 byte | 121.122 byte |
| Toplam CLI wall süresi, yükleme dahil | 1,949 saniye | 1,930 saniye |
| Process tepe private commit | yaklaşık 0,849 GiB | yaklaşık 0,849 GiB |
| Delta işlem sırası | 1 | 2 |

Her ikisinde de base SHA256'sı aynı kaldı; bekleyen metadata yenilemesi 0'dı. İlk aşamadaki canlı refresh 8,831 saniye sürmüş, yaklaşık 1,38 GB yazmış ve 2,572 GiB tepe private commit kullanmıştı. **Canlı olay sayıları/batch'ler farklı olduğundan bu iki canlı koşudan kesin hız katsayısı çıkarılmadı.**

Son teslim binary'si ayrıca üçüncü native append ve ayrı süreçten fixture aramasıyla doğrulandı: işlem sırası 3, base hash yine aynı ve fixture yolları/boyutları doğruydu.

## Dayanıklılık ve kontroller

- `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo test --workspace --locked`, release binary ve örnek derlemeleri geçti.
- **22 test geçti**, ignored/başarısız test yok; doc-test blokları 0 test içerir.
- Sorgu matrisinde tekrar eden adlar, Unicode, dosya/klasör, extension/min-size/largest/limit birleşimleri ve oluşturma/silme/yenileme katmanı doğrusal oracle ile karşılaştırıldı.
- Tamamlanmamış ilk ve ikinci delta frame'leri, ilk sağlam commit'in korunması, kuyruğun temizlenip yeniden eklenmesi, checksum bozulması, eski writer state, farklı base kimliği ve eşzamanlı yazıcı reddi test edildi.
- Log 64 MiB ile sınırlı. Sınırda otomatik büyük rewrite yerine yeni yola açık compaction gerekir. Base + delta beraber korunur; 0.1.0 CLI delta'yı okumaz.
- Gerçek güç kesintisi, süreç öldürerek crash acceptance, uzun süreli daemon, hosted CI ve Everything karşılaştırması çalıştırılmadı. GUI yoktur.

## Kaynak kimliği ve yerel kanıtlar

Eski binary SHA256: `3774F647E8663C76CB069E7433F147BC2E7CFABE659A723C739F9CF2E518D33A`.

Son teslim binary SHA256: `E43FCA23CBD078E8C0B6CA302CAEF8A98607E9BAC85A40A265DC741B3B8EB4C2`.

Karşılaştırma: [yerel run klasörü](../.temp_files/optimization-20261116/).

- [Eski sürümün yeniden ölçümü](../.temp_files/optimization-20261116/baseline-original.json)
- [Son indexed sorgu ölçümü ve linear doğrulaması](../.temp_files/optimization-20261116/indexed-final.json)
- [12.285.803 kaydın alan bazında doğrulaması](../.temp_files/optimization-20261116/logical-record-verification.txt)
- [Aynı 128 değişiklikte eski yazım](../.temp_files/optimization-20261116/update-final-rewrite.json) ve [delta yazımı](../.temp_files/optimization-20261116/update-final-delta.json)
- [Canlı NTFS kabulü](../.temp_files/ntfs-optimized-1794857366388/acceptance.json), [ilk refresh](../.temp_files/ntfs-optimized-1794857366388/refresh.json), [ikinci refresh](../.temp_files/ntfs-optimized-1794857366388/refresh-again.json)
- [Teslim binary'si ile son native doğrulama](../.temp_files/ntfs-optimized-1794857366388/final-binary.json)

Kaynak ve binary hash manifesti karşılaştırma klasöründe saklanır. Büyük kontrollü update benchmark çıktı snapshot'ları son kontrolden sonra kaldırılır; JSON kanıtları, frozen comparison snapshot ve gerçek native base+delta korunur.
