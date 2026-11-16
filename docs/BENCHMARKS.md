# RustSearch — Performans ve doğrulama protokolü

Durum: ölçüm planı. Aşağıdaki sayılar mühendislik hedefidir; gerçekleşmiş sonuç değildir.

İlk gerçek disk ölçümleri ayrı [A aşaması sonuç raporunda](PHASE_A_RESULTS.md) yer alır. Hedef tablosu gözlenen sonuçlarla karıştırılmamalıdır.

## 1. Başarı tanımı

Önce aynı kapsamda doğru sonuç, sonra hız ve kaynak tüketimi. Eksik indeks, atlanan dosyalar, farklı arama anlamı veya gecikmiş güncelleme performans kazancı sayılmaz.

Everything karşılaştırması aynı cihaz, aynı disk, aynı kullanıcı görünürlüğü, aynı dosya kümesi ve eşdeğer metadata/sıralama seçenekleriyle yapılır. Test günündeki kararlı sürüm ile mevcutsa ayrı 1.5 kanalı, sürüm ve kanal açıkça kaydedilerek ayrı raporlanır. Aralarında sonuç birleştirilmez.

Disk analizi için WizTree ayrı referanstır; adı, sürümü, ayrıcalığı, mantıksal/ayrılmış alan seçimi ve hard-link politikası kaydedilir. WizTree'nin export belgeleri hard linkler için farklı alan raporlamasını açıklıyor. [Resmî kılavuz](https://diskanalyzer.com/guide)

## 2. Başlangıç hedefleri

Referans profil: Windows 11 x64, 16 GiB RAM, NVMe, 1 milyon dosya/klasör adı girdisi; başlangıçta ortalama 24 UTF-16 kod birimi ad ve yaklaşık %10 klasör varsayımı. Gerçek ad uzunluğu dağılımı, hard-link sayısı ve indeks seçenekleri raporda bulunur. Donanım ve profil değişirse sonuçlar ayrı tutulur.

| Metrik | İlk hedef | Ölçüm sınırı |
| --- | --- | --- |
| Sıcak sorgu, ilk 100 sonuç | p95 ≤ 10 ms | Çekirdek API; aynı sorgu anlamı ve sıralama |
| Yazımdan görünür sonuçlara | p95 ≤ 50 ms | UI kuyruğu ve çizim dahil; debounce dahil |
| Büyük dosya ilk 100 | p95 ≤ 20 ms | Boyut indeksi hazır, sıralama eşdeğer |
| Hazır klasör toplamına erişim | p95 ≤ 5 ms | Veri erişimi; bütün harita çizimi değil |
| Hazır indeksten pencere açılışı | p95 ≤ 300 ms | Çekirdek çalışırken kısayoldan ilk kullanılabilir kareye |
| Journal güncelleme gecikmesi | p95 ≤ 500 ms | Sonlu mutasyon/kapanıştan sorguda görünmeye; aktif yazmalar ayrıca |
| Boşta CPU | Tek çekirdeğin ortalama <%0,1'i | 10 dakika durağan disk, bütün RustSearch süreçleri |
| Toplam private commit | ≤ 200 MiB | 1 milyon girdide UI + çekirdek + yardımcı, normal çalışma |
| Toplam resident bellek | ≤ 300 MiB | Shared mapping iki kez sayılmadan; ölçüm yöntemi belirtilir |
| İsim indeksinin ilk kurulumu | ≤ 15 saniye fizibilite hedefi | Tanımlı NVMe fixture; boyut indeksleme süresi ayrı |

Bellek için aday dağılım: çekirdek/indeks 140 MiB, görünür UI 40 MiB, yardımcı 20 MiB. Bu dağılım tasarım bütçesidir. Ad havuzu, arama anahtarları ve ek indeksler bu bütçeye dahildir. Büyük fixture'larda girdi başına byte ve toplam kullanım raporlanır; 1 milyon hedefine uymak daha büyük ölçek başarısını kanıtlamaz.

“Daha az kaynak” iddiası yalnız Everything'in eşdeğer kapsamındaki gerçek ölçümüyle karşılaştırıldıktan sonra kullanılabilir. Hedef bütçeyi tutturmak tek başına rakipten daha düşük tüketim demek değildir.

## 3. Ayrı ölçülecek işler

- İlk isim indeksi süresi ve hazır olan kayıt kapsamı.
- İlk tam boyut indeksi ve klasör toplamlarının hazır olma süresi.
- Çekirdek kapalıyken snapshot yükleme + journal yakalama.
- Çekirdek açıkken UI açılışı.
- Çekirdek sorgu gecikmesi ve görünür UI gecikmesi.
- İlk sonuç, kesin toplam sonuç sayısı ve tam sıralamanın bitişi.
- İndeks normal çalışması, olay fırtınası ve snapshot birleştirmesi sırasındaki tepe bellek.
- Toplam CPU zamanı, disk okuma/yazma byte'ları, indeks dosya boyutu ve güncelleme borcu.

İndeks için kaydedilmiş skor, sorgu için kaydedilmiş skor yerine kullanılamaz. Private commit, working set ve file-backed cache ayrı raporlanır; memory-mapped indeks düşük private değer gösteriyor diye bedelsiz kabul edilmez.

## 4. Veri kümeleri ve sorgular

En az 100 bin, 1 milyon ve 10 milyon girdilik sentetik veri; ayrıca izinli gerçek çalışma diski. Sentetik veride sabit seed ve fixture sürümü bulunur. NVMe, SATA SSD ve HDD ayrı raporlanır; donanım erişilemiyorsa o profil denenmemiş sayılır.

Sorgu grupları:

1. Tam ad, nadir substring, yaygın substring ve eşleşmeyen ad.
2. Bir/iki karakterlik çok geniş sorgular ve boş sorgu.
3. Prefix, birden fazla terim ve yol filtresi.
4. Uzantı + boyut + tarih kesişimi.
5. Tüm dosyaları boyuta göre sıralama ve top-100.
6. Klasör seçimi, en büyük alt klasörler ve treemap drill-down.
7. Türkçe i/İ/ı/I, emoji, birleşik Unicode ve uzun yollar.
8. Arama yazılırken yoğun dosya oluşturma/silme/taşıma.

Her sorgu için beklenen sonuç kimlikleri ve sıralama/tie-break sözleşmesi bulunur. İlk 100 sonucu gösteren uygulama bütün sonuçları tamamlayan uygulamayla aynı metrik altında karşılaştırılmaz. Fuzzy/regex ilk kapının zorunlu özelliği değildir.

## 5. Tekrar ve raporlama

- Release derlemesi; toolchain, commit, OS build, CPU, RAM, disk ve güvenlik yazılımı durumu kaydedilir.
- Yarışmacılar aynı anda çalıştırılmaz; koşu sırası değiştirilir ve indeks kapsamları doğrulanır.
- Warm sorgularda grup başına en az 1.000 örnek; warm-up örnekleri ayrı tutulur. 30 örnekle p99 iddiası kurulmaz.
- Başlangıç/indeksleme deneylerinde en az 10 tekrar; medyan, min/max ve örnek sayısı raporlanır. p95/p99 için yetersiz örnek açıkça belirtilir.
- OS cache soğukluğu yalnız süreç yeniden başlatılarak varsayılmaz. Gerçek cold koşul kurulmadıysa “process-cold / cache bilinmiyor” diye adlandırılır.
- Güç modu, Defender davranışı ve throttling kaydedilir; bir rakibe özel istisna uygulanmaz.
- Sorgu motorları mümkünse CLI/SDK ile karşılaştırılır; UI sonuçları ayrı uçtan uca senaryoda ölçülür.
- Ortalama, p50, p95 ve yeterli örnekte p99; bellek için normal ve tepe değerler sunulur.
- Sonuçlar CSV/JSON ve çalıştırma komutlarıyla tekrar üretilebilir tutulur. Bu depoda henüz bir benchmark runner yoktur.

## 6. Doğruluk kabul örnekleri

- Dosya oluşturma, büyütme, küçültme, silme; açık dosyada süren yazma.
- Tek dosya rename/move, büyük klasör taşıma, parent'ın önce/sonra görülmesi.
- Hard link oluşturma/silme; kalan bağlantı varken alanı serbest saymama.
- Sparse ve sıkıştırılmış dosyalar; alternate data streams; resident küçük dosya.
- Junction/symlink döngüleri ve başka birime giden bağlantılar.
- İzin reddi, kullanıcı değişimi, erişim yetkisinin sonradan kaldırılması.
- Bulut placeholder'larının içerik indirilmeden işlenmesi.
- İndeks oluşturulurken eşzamanlı değişiklik; journal wrap/recreate.
- Uyku/uyanma, disk çıkarma/takma ve sürücü harfinin değişmesi.
- Checkpoint ile snapshot arasındaki her kritik noktada süreç kesilmesi.
- MFT referansının yeniden kullanımı; bozuk/yarım snapshot'ın reddi.

Küçük ve kontrol edilen fixture'larda bağımsız Win32 enumerasyonu + metadata sorguları oracle olur. Rakip uygulama tek doğruluk kaynağı sayılmaz. Aktif diskte tam atomik anlık görüntü iddiası kurulmaz; doğruluk kontrolü durağan fixture veya tanımlı değişiklik bariyerinde yapılır.

## 7. İlerleme kapıları

1. Doğruluk fixture'ları geçmeden hız sonucu yayımlanmaz.
2. İlk fizibilite raporu ad indeksleme ve boyut tamamlama darboğazlarını ayırır.
3. Rakibi geçmeyen sorgu grupları rapordan çıkarılmaz.
4. 2 kat hız hedefi tutmazsa veri yerleşimi/sorgu stratejisi yeniden değerlendirilir; sonuç gizlenmez.
5. UI, yardımcı süreç ve snapshot birleştirmesi ölçülmeden yalnız çekirdek sonucuna dayanarak düşük kaynak iddiası yapılmaz.
