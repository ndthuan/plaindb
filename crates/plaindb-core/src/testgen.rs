use std::fs;
use std::path::Path;

use serde_json::{Value, json};

use crate::codec::{Doc, codec_for};
use crate::config::{Format, IndexSpec};
use crate::error::Result;
use crate::store::Store;

pub const DB: &str = "bench";
pub const COLLECTION: &str = "items";
const STAMP: &str = "2026-01-01T00:00:00.000Z";

const CITIES: [(&str, &str); 8] = [
    ("Hanoi", "VN"),
    ("Hue", "VN"),
    ("Saigon", "VN"),
    ("Lyon", "FR"),
    ("Porto", "PT"),
    ("Osaka", "JP"),
    ("Austin", "US"),
    ("Bergen", "NO"),
];

const STATUSES: [&str; 6] = [
    "draft",
    "review",
    "approved",
    "published",
    "archived",
    "rejected",
];

const TAGS: [&str; 40] = [
    "alpha", "beta", "gamma", "delta", "kernel", "driver", "harbour", "market", "garden", "bridge",
    "river", "mountain", "winter", "summer", "autumn", "spring", "copper", "silver", "carbon",
    "cobalt", "orbit", "signal", "ledger", "atlas", "anchor", "beacon", "canvas", "cipher",
    "ember", "falcon", "glacier", "island", "lantern", "meadow", "nebula", "orchard", "prairie",
    "quartz", "summit", "tundra",
];

const ENGLISH: [&str; 12] = [
    "The harbour opens before dawn and the first boats leave without a sound.",
    "Every record in the ledger was checked twice against the original notes.",
    "A small team can keep a large collection tidy when the rules are simple.",
    "The report explains why the migration took longer than anyone expected.",
    "Readers asked for shorter summaries, so the editors trimmed each section.",
    "Wind from the north kept the market quiet through most of the morning.",
    "The archive holds letters, maps and a few photographs of the old bridge.",
    "Nobody remembered who first suggested moving the shelves to the east wall.",
    "A careful index turns a slow search into an answer that arrives at once.",
    "The committee approved the plan with two amendments and a long list of thanks.",
    "Backups were restored on a quiet Sunday and every file matched its checksum.",
    "The garden produced more pears than the neighbours could eat in a month.",
];

const VIETNAMESE: [&str; 12] = [
    "Buổi sáng ở bến cảng bắt đầu từ rất sớm, khi những chiếc thuyền đầu tiên rời bờ.",
    "Mỗi bản ghi trong sổ cái đều được kiểm tra hai lần so với ghi chép gốc.",
    "Một nhóm nhỏ vẫn giữ được bộ sưu tập lớn gọn gàng nếu quy tắc đủ đơn giản.",
    "Báo cáo giải thích vì sao việc chuyển dữ liệu mất nhiều thời gian hơn dự kiến.",
    "Độc giả muốn bản tóm tắt ngắn hơn nên biên tập viên đã cắt bớt từng phần.",
    "Gió bắc làm khu chợ vắng lặng gần như suốt buổi sáng hôm đó.",
    "Kho lưu trữ có thư từ, bản đồ và vài tấm ảnh chụp cây cầu cũ.",
    "Không ai nhớ ai là người đầu tiên đề nghị dời các kệ sách sang bức tường phía đông.",
    "Một chỉ mục cẩn thận biến cuộc tìm kiếm chậm chạp thành câu trả lời có ngay.",
    "Hội đồng thông qua kế hoạch với hai điểm sửa đổi và một danh sách lời cảm ơn dài.",
    "Bản sao lưu được khôi phục vào một chiều Chủ nhật yên tĩnh và mọi tệp đều khớp.",
    "Khu vườn cho nhiều lê hơn số mà hàng xóm có thể ăn trong một tháng.",
];

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut value = self.0;
        value = (value ^ (value >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        value ^ (value >> 31)
    }

    fn below(&mut self, bound: usize) -> usize {
        usize::try_from(self.next() % u64::try_from(bound).expect("bound fits")).expect("fits")
    }
}

pub fn id_for(index: usize) -> String {
    format!("doc-{index:06}")
}

fn body(rng: &mut Rng, format: Format) -> String {
    let target = 700 + rng.below(2900);
    let vietnamese = format == Format::Md && rng.below(2) == 0;
    let sentences = if vietnamese { &VIETNAMESE } else { &ENGLISH };
    let mut text = String::new();
    while text.len() < target {
        if !text.is_empty() {
            text.push(if rng.below(6) == 0 { '\n' } else { ' ' });
        }
        text.push_str(sentences[rng.below(sentences.len())]);
    }
    text
}

pub fn document(format: Format, seed: u64, index: usize) -> Doc {
    let mut rng = Rng(seed ^ (u64::try_from(index).expect("index fits") << 20));
    let (city, country) = CITIES[rng.below(CITIES.len())];
    let tag_count = 1 + rng.below(4);
    let tags: Vec<&str> = (0..tag_count)
        .map(|_| TAGS[rng.below(TAGS.len())])
        .collect();
    let age = 18 + rng.below(63);
    let score = f64::from(u32::try_from(rng.below(10_000)).expect("fits")) / 100.0;
    let status = STATUSES[rng.below(STATUSES.len())];
    let active = rng.below(3) != 0;
    let zip = 10_000 + rng.below(89_999);
    let title = format!(
        "{} {} {}",
        TAGS[rng.below(TAGS.len())],
        TAGS[rng.below(TAGS.len())],
        index
    );
    let value = json!({
        "name": format!("Person {index}"),
        "email": format!("user{index}@example.test"),
        "age": age,
        "score": score,
        "active": active,
        "status": status,
        "tags": tags,
        "address": { "city": city, "country": country, "zip": zip.to_string() },
        "title": title,
        "body": body(&mut rng, format),
    });
    match value {
        Value::Object(map) => map,
        _ => unreachable!("the template is an object"),
    }
}

pub fn indexes() -> Vec<IndexSpec> {
    vec![
        IndexSpec::Field {
            field: "email".to_owned(),
            unique: true,
            multi: false,
        },
        IndexSpec::Field {
            field: "age".to_owned(),
            unique: false,
            multi: false,
        },
        IndexSpec::Field {
            field: "status".to_owned(),
            unique: false,
            multi: false,
        },
        IndexSpec::Field {
            field: "address.city".to_owned(),
            unique: false,
            multi: false,
        },
        IndexSpec::Field {
            field: "tags".to_owned(),
            unique: false,
            multi: true,
        },
        IndexSpec::Text {
            text: vec!["title".to_owned(), "body".to_owned()],
        },
    ]
}

pub fn build_store(
    root: &Path,
    format: Format,
    count: usize,
    seed: u64,
    with_indexes: bool,
) -> Result<Store> {
    let store = Store::init(root)?;
    store.create_db(DB)?;
    store.create_collection(DB, COLLECTION, format)?;
    if with_indexes {
        store.set_indexes(DB, COLLECTION, indexes())?;
    }
    let dir = store.collection_dir(DB, COLLECTION)?;
    let codec = codec_for(format);
    for index in 0..count {
        let mut doc = document(format, seed, index);
        let id = id_for(index);
        doc.insert("_id".to_owned(), Value::String(id.clone()));
        doc.insert("_created_at".to_owned(), Value::String(STAMP.to_owned()));
        doc.insert("_updated_at".to_owned(), Value::String(STAMP.to_owned()));
        fs::write(
            dir.join(format!("{id}.{}", format.extension())),
            codec.encode(&doc)?,
        )?;
    }
    store.rescan()?;
    Ok(store)
}
