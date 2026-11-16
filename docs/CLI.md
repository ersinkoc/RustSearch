# RustSearch CLI — İlk aşama

Windows'a özel 0.0.1 fizibilite prototipi. Uygulama kullanıcı dosyalarını değiştirmez; indeks, delta, türetilebilir arama cache'i ve rapor yazar. Kabul script'i yalnız oluşturduğu test dosyalarını değiştirir. Ölçümler: [arama/delta](OPTIMIZATION_RESULTS.md), [kalıcı cache/başlangıç](CACHE_RESULTS.md), [snapshot/cache yükleme](LOAD_RESULTS.md).

## Derleme ve kontroller

Geliştirme ortamında Rust 1.93.0, MSVC hedefi kullanıldı. Yeni bir toolchain kurmak gerekmez; mevcut ortam yeterlidir.

```powershell
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo build --release --locked
```

Çalıştırılabilir dosya: `target\release\rustsearch.exe`.

Workspace:

| Crate | Görev |
| --- | --- |
| core | Tam dosya referansları, paketli kayıtlar, UTF-16 ad havuzu, arama anahtarları ve path çözümleme |
| windows-indexer | Win32 API'leri, NTFS/USN enumerasyonu, normal klasör taraması, metadata boyut sorguları ve süreç sayaçları |
| query | Sıralı ad grupları, karakter/trigram aday özetleri, uzantı ve bilinen boyut sıralamaları |
| storage | CRC32 kontrollü binary snapshot + sınırlı delta log; commit/checkpoint, yazıcı kilidi ve Windows DACL |
| cli | Komutlar, JSON raporlama, warm benchmark ve idle ölçümü |

## İki ayrı indeksleme yolu

```powershell
New-Item -ItemType Directory -Path .temp_files -Force

# Yönetici yetkisi gerektirmez; reparse hedeflerini izlemez.
.\target\release\rustsearch.exe index --root D:\Projects --sizes --output .temp_files\projects.rsidx --report .temp_files\projects-index.json

# Aşağıdakiler için yönetici PowerShell gerekir.
.\target\release\rustsearch.exe probe --volume D:
.\target\release\rustsearch.exe index --volume D: --output .temp_files\native.rsidx --report .temp_files\native-index.json

# İlk 5000 uygun dosya nesnesinde boyut toplama maliyetini ölç.
.\target\release\rustsearch.exe index --volume D: --sizes --size-limit 5000 --output .temp_files\sampled.rsidx
```

`--volume` yalnız `FSCTL_ENUM_USN_DATA` yolunu kullanır. Yetki yoksa hata verir; fark ettirmeden klasör taramasına geçmez. Mevcut journal kullanılır; journal oluşturulmaz, silinmez veya yeniden yapılandırılmaz.

`--root` normal Windows dizin taramasıdır; dosya referansları yerine snapshot'a özel sentetik ID kullanır. NTFS üzerinde çalışsa da native MFT/USN hızı diye raporlanmaz. Dizin taramasında alınamayan öğeler `skipped_entries`, takip edilmeyen reparse öğeleri ayrı sayaçta görünür.

Native `--sizes` bütün uygun dosyalarda mantıksal boyut sorgular. `--size-limit` örneklemesi rastgele değildir: enumerasyon/ID sırasındaki ilk uygun dosyalardır. İstatistiksel disk örneklemesi veya bütün diskin alan dağılımı olarak yorumlanmaz. Kalan dosyalar sıfır boyutlu değil, bilinmiyor durumundadır.

## Arama

```powershell
.\target\release\rustsearch.exe search --index .temp_files\native.rsidx --query report
.\target\release\rustsearch.exe search --index .temp_files\native.rsidx --query README --mode prefix --kind file
.\target\release\rustsearch.exe search --index .temp_files\native.rsidx --query Cargo.toml --mode exact --json
.\target\release\rustsearch.exe search --index .temp_files\sampled.rsidx --ext iso --min-size 1GiB --json
.\target\release\rustsearch.exe search --index .temp_files\sampled.rsidx --largest --limit 100 --json
```

Filtreler AND ile birleşir. İlk prototipte ürün planındaki serbest `ext:pdf` parser'ı yerine açık CLI seçenekleri kullanılır. Yol içinde arama, tarih, regex ve fuzzy henüz yoktur. Adlar Unicode lowercase ile eşleştirilir; locale, Unicode normalizasyonu ve Windows ordinal karşılaştırması eşdeğeri değildir. Örneğin `İ`, `i` + combining dot olarak katlanır. Özgün UTF-16 ad ve tam dosya kimliği ayrı korunur; JSON insan gösterimi bazı adlarda kayıplı olabilir ve bu durum işaretlenir.

Sonuçlar dosya ID sırasıyla gelir; `--largest` boyut azalan, eşitlikte ID artan sıralamadır. Bütün eşleşmeler sayılır, en çok `--limit` satır döner. Süreye tam sayım ve sonuç seçimi dahildir; snapshot yükleme, arama yardımcılarının hazırlanması ve path çözümleme ayrıdır. Bu nedenle ölçüm “ilk sonuç bulunduğu an” değildir.

Varsayılan `--strategy indexed` yolunda tam ad/prefix/uzantı için binary lookup, substring için benzersiz ad grupları ve yanlış negatif üretmeyen karakter/trigram aday özetleri kullanılır. Ardından gereken durumda gerçek ad eşleşmesi kontrol edilir. Dosya/kayıt sayısını azaltarak hız kazanılmaz. `--strategy linear`, karşılaştırma için eski doğrusal sorgu anlamını korur. Yapılandırılmış filtre kombinasyonlarının sonuç ID'leri iki yol arasında test edilir.

Arama yardımcıları `.rsidx.search` dosyasında kalıcıdır. `index` ve yeni yola `compact` varsayılan olarak cache'i bir kez oluşturur; sonrasındaki ilk ayrı CLI sorgusu cache-hit olur. Eski 0.1/0.2 snapshot'ında cache yoksa ilk sorgu onu oluşturur. JSON'da `cache.state`, `read_ms`, `build_ms`, `write_ms`, `delta_bind_ms` ve toplam `prepare_ms`; metin çıktısında `prepare` ve cache durumu görünür.

`search`, `bench`, `idle` komutlarında `--no-cache` cache okumadan/yazmadan yeniden hazırlamayı seçer. `index` ve `compact` üzerinde aynı seçenek tek seferlik cache hazırlığını atlar. Cache yoksa yeniden oluşturma maliyeti hâlâ vardır; maliyet ortadan kalkmış gibi raporlanmaz. Geçerli cache okunurken checksum, boyutlar, kayıt üyeliği, maske ve sıralama/kapsam denetimleri yapılır; hazırlık sıfır değildir.

Cache sürümü ve immutable base header kimliği eşleşmelidir. Cache yalnız base satır numaraları/ad grupları/boyut sıralamalarını tutar. Güncel delta'lar okununca değişen eski satırlar maskelenir, grup sayımları düzeltilir ve güncel kayıtlar küçük bir katmanda sorgulanır. Böylece yeniden adlandırma, silme, uzantı/boyut/tür değişimi ve yeni dosyalar eski cache'ten yanlış sonuç üretmez. `cache.bound_sequence` hangi committed delta görünümünün bağlandığını gösterir. Delta sayısı arttıkça bu küçük katmanın sorgu maliyeti de artar; gerektiğinde `compact` kullanılır.

Cache bozuk, eksik, eski formatta veya başka base nesline aitse yeniden kurulur. Yazma mümkün değilse ya da başka cache oluşturucu kilidi tutuyorsa arama geçerli source üzerinde bellekte kurularak devam eder (`built_unsaved`). Kaynak snapshot/delta bozuksa bu tolerans uygulanmaz; source hatası hâlâ komutu durdurur. Cache atomik temp+sync+replace ile, mevcut kullanıcı/SYSTEM/Administrators DACL'i altında yayımlanır; yarım cache yayımlanmaz.

Blok yükleyici snapshot kayıtlarını, UTF-16 adları ve cache tablolarını 256 KiB'lık yeniden kullanılan blokla ayrıştırır; checksum için ikinci dosya geçişi yapmaz. Snapshot kayıt sınırları ve ID sırası ayrıştırma sırasında kontrol edilir; cache üyelik/maske/sıralama/kapsam denetimleri sürer. CRC ve dosya sonu doğrulanmadan indeks sorguya açılmaz. Snapshot ve delta formatları değişmedi. Yeni CLI, cache v1'i bütün eski kontrolleriyle okur ve kilit alınabiliyorsa bir kez v2'ye çevirir (`upgraded`, `build_ms=0`); kilit/yazma mümkün değilse doğrulanmış eski cache kullanılır (`legacy_hit`). Dönüşüm süresi `write_ms` içinde görünür. Eski deney binary'leri cache v2'yi okuyamaz; karşılaştırma için ayrı cache yolları kullanılır. Bellek eşleme kullanılmaz ve bütün encoded payload geçici bir belleğe kopyalanmaz.

Boyutu bilinmeyen dosyalar boyut filtreleri ve largest sorgusunda dışarıda kalır; rapordaki `unknown_size_files` kapsamı kontrol edilmelidir. `logical_size: 0` ile `logical_size: null` farklıdır. Güncellenemeyen mevcut kayıtların boyutu bilinmiyor yapılır; varsa son bilinen adı korunur ve `pending_refresh` olarak işaretlenir.

## USN güncellemesi

```powershell
# Yönetici PowerShell. Ana snapshot sabit kalır, küçük bir delta eklenir.
.\target\release\rustsearch.exe refresh --index .temp_files\native.rsidx --sizes --report .temp_files\refresh.json
```

Başlangıçta yakalanan USN sınırından sonra olan değişiklikler yeniden okunur. Güncel ad/parent bilgisi dosya ID'siyle açılan nesneden alınır; resize için ayrıca mantıksal boyut sorgulanır. Journal kimliği veya geçerli aralık değişmişse eski checkpoint kabul edilmez ve yeniden indeksleme istenir.

Okunamayan kayıtlar `pending_refresh` listesinde saklanır ve sonraki `refresh` işleminde tekrar denenir. Hata kodu dağılımı raporda vardır. Journal checkpoint'inin ilerlemesi bütün metadata'nın eksiksiz güncel olduğu anlamına gelmez. Açık disk üzerindeki sonuçlar atomik volume snapshot değildir.

Her refresh yalnız değişen/silinen kayıtları ve yeni checkpoint'i `native.rsidx.delta` dosyasına ekler. Baz kayıtlar sabit kalır; güncel nesneler küçük bir bellek katmanında eskilerini gölgeler. Ad havuzlarına yüklemede toplam 192 KiB boşluk bırakılır; küçük güncellemeler büyük havuzları yeniden tahsis etmez. Büyük/uzun bir delta zinciri bu boşluğu aşabilir; havuz büyümesi maliyetsiz değildir.

Delta log ana snapshot'ın header kimliğine bağlıdır. Her işlem sıra numarası, önceki checkpoint, CRC32 ve commit işareti taşır. Tamamlanmamış son çerçeve checkpoint'i ilerletmez; sonraki yazıcı bu kuyruğu kesip tekrar ekler. Tam bir commit'te checksum/format/kimlik hatası varsa dosya sessizce kabul edilmez. `.rsidx.lock` üzerinde beklemesiz tek yazıcı kilidi, load → journal toplama → append boyunca tutulur. Dosya `sync_all` tamamlanmadan komut başarı raporlamaz. Gerçek güç kesintisi testi yapılmadı.

Log bütçesi 64 MiB'dir. Sınıra gelindiğinde otomatik büyük yeniden yazım yapılmaz; açıkça yeni bir snapshot'a compaction istenir:

```powershell
# Yeni bir çıktı adı gerekir; kaynak snapshot ve delta korunur.
.\target\release\rustsearch.exe compact --index .temp_files\native.rsidx --output .temp_files\compacted.rsidx --report .temp_files\compact.json
```

Compaction güncellemeleri birleştirir ve yinelenen özgün UTF-16 adlar ile normalize anahtarları paylaşır. Başka bir dosyaya yazdığı için yarıda kesilen işlem kaynak base+delta çiftini değiştirmez. Var olan bir delta log varken `index --output` ile aynı base üzerine yeniden yazmak reddedilir. Sürekli servis ve otomatik compaction henüz yoktur.

**Taşıma/yedekleme:** Base ve `.delta` birlikte bir indekstir; ikisini aynı nesilden koruyun. `.lock` yalnız koordinasyon dosyasıdır. `.search` türetilebilir; taşınması zorunlu değildir. İçerik kimliği aynı base ile eşleşirse kullanılabilir, farklı nesilde reddedilir. `.search.lock` cache oluşturucularını koordine eder. 0.1.0 CLI delta dosyasını okumaz; yeni indekslerde güncel CLI kullanın. Tek source dosyalı aktarım için önce yeni bir yola `compact` çalıştırın.

Cache v2, grup sırasındaki posting listesi yerine her base satırının grup numarasını diskte tutar; dosya boyutu değişmez. Yükleyici source kayıtlarını ID sırasıyla kontrol ederken aynı posting listelerini oluşturur. Grup aralıkları, temsilci ID, üyelik/tür, maske, tam kapsam, uzantı tekilliği ve boyut sıralaması doğrulanır. Delta görünümü bu doğrulanmış immutable base tablosuna ayrıca bağlanır.

## Kaynak ölçümü

```powershell
.\target\release\rustsearch.exe info --index .temp_files\native.rsidx
.\target\release\rustsearch.exe bench --index .temp_files\native.rsidx --iterations 1000 --warmup 10 --report .temp_files\bench.json
.\target\release\rustsearch.exe bench --index .temp_files\native.rsidx --iterations 100 --warmup 5 --verify-linear --report .temp_files\verified-bench.json
.\target\release\rustsearch.exe idle --index .temp_files\native.rsidx --seconds 10 --report .temp_files\idle.json
```

Benchmark dokuz sabit sorgu grubunu çalıştırır: nadir/yaygın/eşleşmeyen substring, prefix, tam ad, uzantı, büyük dosyalar, largest ve bütün kayıtlar. Her grupta warm-up ölçümden çıkarılır. 100'den az örnekte p95, 1000'den az örnekte p99 raporlanmaz. `--verify-linear`, her grubun tam sayımını ve dönen ID sıralamasını ölçüm dışında referans sorguyla karşılaştırır. Query loop'u disk okumaz; snapshot yükleme ve arama indekslerinin hazırlanması ayrı raporlanır. OS cache kontrol edilmediğinden cold-disk sonucu değildir.

Süre: wall clock; CPU: Windows process kernel+user zamanı, tek çekirdeğe göre yüzde. Bellek: private commit, working set ve process-lifetime tepe değerleri; payload arena byte'ları ayrıca raporlanır. Commit ve resident bellek aynı metrik değildir.

I/O sayaçları süreç transfer sayaçlarıdır; filesystem cache veya fiziksel disk trafiği ölçümü değildir. Metadata IOCTL trafiği read transfer yerine other transfer kategorisine yansıyabilir. Rapor yazımı ve stdout çıktı maliyeti kaynak ölçümünden sonra gelir.

`idle`, yüklenmiş indeks ve hazırlanmış arama yardımcılarını bellekte tutarak CLI'nın uyumasını ölçer. Arka plan USN okuyucusu/servisi olmadığı için bu sonuç sürekli güncelleme yapan nihai uygulamanın boşta tüketimini kanıtlamaz. Kısa deneylerde CPU zamanlayıcı çözünürlüğü nedeniyle CPU değeri sıfır olabilir; 10 dakika hedefinin yerine geçmez.

## Gerçek NTFS kabul testi

Önce release derleyin; aşağıdaki script'i yönetici PowerShell'de çalıştırın. D: örneğinde proje ve fixture aynı birimde olmalıdır.

```powershell
.\scripts\ntfs-acceptance.ps1 -Volume D:
```

Script tüm birimin canonical ad indeksini oluşturur, 5000 boyut denemesi yapar, proje `.temp_files` altında kendi fixture'ını oluşturur. İlk enumerasyonda fixture dosyalarını arar; ardından rename, move, resize, create ve delete uygular. Journal replay sonrasında yolları ve dosya boyutlarını bağımsız `Get-ChildItem` enumerasyonu ile karşılaştırır; Unicode ve sıfır byte dosyası da kapsanır. İki ardışık refresh sonrasında base SHA256'sının değişmediği ve delta sırasının süreçler arasında devam ettiği doğrulanır. Durum, binary SHA256, makine bilgisi ve JSON raporları run klasörüne yazılır. İlk başarısız koşu kanıtı üzerine yazılmaz.

0.3 kabulü ayrıca indeks oluşturmanın cache'i önceden hazırladığını, ilk sorgunun cache-hit olduğunu ve iki delta neslinde hem cache-hit hem doğru güncel sonuçlar alındığını kontrol eder. Base ve search-cache SHA256'ları delta'larda sabit kalmalıdır. Güncellenen şey cache'in immutable tablosu değil, sorgunun bağladığı delta görünümüdür.

Süreç başlangıcı karşılaştırması için `scripts/benchmark-startup.ps1`, korunmuş eski executable ve bir frozen snapshot alır. Her varyantı yeni süreçte aynı CPU affinity ile dönüşümlü çalıştırır; min/medyan/max raporlar. Base, varsa delta ve cache hash'lerinin sabitliğini; örnekler arasında kayıt sayısını ve delta sırasını kontrol eder. Dosya/OS cache'lerini boşaltmaz; bu cold-disk testi değildir. Son karşılaştırma [CACHE_VALIDATION_RESULTS.md](CACHE_VALIDATION_RESULTS.md), önceki çalışmalar [LOAD_RESULTS.md](LOAD_RESULTS.md) ve [CACHE_RESULTS.md](CACHE_RESULTS.md) içindedir.

Bu fixture doğrulaması bütün diskteki 12 milyon kaydın ayrı ayrı eksiksizliğini kanıtlamaz. Journal sarılması testte saf checkpoint denetimiyle doğrulanır; gerçek journal silinmez/sardırılmaz.

## Kalıcılık, erişim ve sınırlar

- Snapshot sürümü, uzunlukları, CRC32 ve kayıt/arena sınırları doğrulanmadan arama açılmaz.
- Temp dosya aynı dizinde `create_new` ile oluşturulur; flush + sync ardından Windows replace ile yayımlanır.
- Veri yazılmadan önce kalıtımı kapalı DACL uygulanır: mevcut kullanıcı SID'si, SYSTEM ve Administrators. Başka oturumlara hizmet veren IPC/servis yoktur. CRC32 kimlik doğrulama değildir.
- Snapshot dosya adlarını ve dizin yapısını içerir; arama çıktısı da seçilen dosya yollarını gösterebilir. Otomatik kaynak raporları dosya adlarını içermez.
- Native backend bir dosya referansına tek canonical ad tutar. Ek hard-link yolları henüz kapsanmaz; tam Everything eşdeğerliği iddiası yoktur.
- Default data stream'in mantıksal uzunluğu dışında allocated size, compression, sparse allocation ve ADS toplamı henüz hesaplanmaz. Gerçek disk alanı haritası bu ölçüler tamamlanmadan yapılmayacak.
- Native parser V2 USN kayıtlarını destekler; başka sürüm görüldüğünde sessizce yanlış parse etmek yerine reddeder.
- Substring yolu bütün dosyaları taramaz; benzersiz ad gruplarını özetlerle eler. Bu da benzersiz grup sayısıyla büyür; her corpus ve sorgu için sabit süre iddiası yoktur. Ad tekrar oranı bellek kazancını etkiler.
- Everything ile eşdeğer kapsamlı benchmark yapılmadan hız veya RAM üstünlüğü iddiası kullanılmaz.

API kaynakları: [USN enumerasyonu](https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ni-winioctl-fsctl_enum_usn_data), [dosyanın güncel USN kaydı](https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ni-winioctl-fsctl_read_file_usn_data), [process bellek sayaçları](https://learn.microsoft.com/en-us/windows/win32/api/psapi/ns-psapi-process_memory_counters_ex).
