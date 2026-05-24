use rand::Rng;
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicU64, Ordering};
use regex::Regex;

// ═══════════════════════════════════════════════════════════
// Section: Hash Functions
// ═══════════════════════════════════════════════════════════

/// Computes a DJB2 hash of a string, returning a signed 32-bit integer.
/// Deterministic across runtimes. Ported from upstream TypeScript hash.ts.
pub fn djb2_hash(s: &str) -> i32 {
    let mut hash: i32 = 0;
    for byte in s.bytes() {
        hash = (hash << 5).wrapping_sub(hash).wrapping_add(byte as i32);
    }
    hash
}

/// Hashes arbitrary content for change detection using SHA-256.
/// Returns a hex string.
pub fn hash_content(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Hashes two strings disambiguating ("ts","code") vs ("tsc","ode").
/// Uses a null separator to ensure different splits produce different hashes.
pub fn hash_pair(a: &str, b: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(a.as_bytes());
    hasher.update(&[0u8]); // null separator
    hasher.update(b.as_bytes());
    format!("{:x}", hasher.finalize())
}

// ═══════════════════════════════════════════════════════════
// Section: Fingerprint
// ═══════════════════════════════════════════════════════════

/// Hardcoded salt for fingerprint validation.
pub const FINGERPRINT_SALT: &str = "59cf53e54c78";

/// Computes a 3-character fingerprint for Claude Code attribution.
/// Algorithm: SHA256(SALT + msg[4] + msg[7] + msg[20] + version)[:3]
pub fn compute_fingerprint(message_text: &str, version: &str) -> String {
    let indices = [4usize, 7, 20];
    let chars: String = indices
        .iter()
        .map(|&i| {
            message_text
                .chars()
                .nth(i)
                .unwrap_or('0')
        })
        .collect();

    let input = format!("{}{}{}", FINGERPRINT_SALT, chars, version);
    let hash = hash_content(&input);
    hash[..3].to_string()
}

// ═══════════════════════════════════════════════════════════
// Section: UUID & Agent ID
// ═══════════════════════════════════════════════════════════

/// Validates if a string is a valid UUID format
pub fn validate_uuid(maybe_uuid: &str) -> Option<&str> {
    if maybe_uuid.is_empty() {
        return None;
    }
    let re = Regex::new(r"(?i)^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$").unwrap();
    if re.is_match(maybe_uuid) {
        Some(maybe_uuid)
    } else {
        None
    }
}

/// Generates a new agent ID with prefix for consistency with task IDs.
/// Format: a{label-}{16 hex chars}
pub fn create_agent_id(label: &str) -> String {
    let suffix = random_hex(8);
    if !label.is_empty() {
        format!("a{}-{}", label, suffix)
    } else {
        format!("a{}", suffix)
    }
}

/// Generates a random hex string of the given byte length
pub fn random_hex(n: usize) -> String {
    let mut rng = rand::thread_rng();
    let bytes: Vec<u8> = (0..n).map(|_| rng.gen::<u8>()).collect();
    format!("{:0width$x}", bytes.iter().fold(0u128, |acc, &b| acc * 256 + b as u128), width = n * 2)
}

// ═══════════════════════════════════════════════════════════
// Section: Tagged ID
// ═══════════════════════════════════════════════════════════

static TAGGED_ID_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Creates a tagged ID string of the form "tag_counter_randomHex"
pub fn to_tagged_id(tag: &str) -> String {
    let counter = TAGGED_ID_COUNTER.fetch_add(1, Ordering::SeqCst);
    let mut rng = rand::thread_rng();
    let random_bytes: Vec<u8> = (0..4).map(|_| rng.gen::<u8>()).collect();
    let random_hex: String = random_bytes.iter().map(|b| format!("{:02x}", b)).collect();
    format!("{}_{}_{}", tag, counter, random_hex)
}

/// Extracts the tag portion from a tagged ID
pub fn parse_tagged_id(id: &str) -> Option<&str> {
    let parts: Vec<&str> = id.splitn(3, '_').collect();
    if parts.len() < 2 {
        return None;
    }
    Some(parts[0])
}

/// Extracts the counter portion from a tagged ID
pub fn get_tagged_id_counter(id: &str) -> Option<u64> {
    let parts: Vec<&str> = id.splitn(3, '_').collect();
    if parts.len() < 2 {
        return None;
    }
    parts[1].parse::<u64>().ok()
}

/// Checks if a string is a valid tagged ID with the given tag
pub fn validate_tagged_id(id: &str, expected_tag: &str) -> bool {
    parse_tagged_id(id).map_or(false, |tag| tag == expected_tag)
}

// ═══════════════════════════════════════════════════════════
// Section: Word Lists & Slug Generation
// ═══════════════════════════════════════════════════════════

const ADJECTIVES: &[&str] = &[
    "abundant", "ancient", "bright", "calm", "cheerful", "clever", "cozy",
    "curious", "dapper", "dazzling", "deep", "delightful", "eager", "elegant",
    "enchanted", "fancy", "fluffy", "gentle", "gleaming", "golden", "graceful",
    "happy", "hidden", "humble", "jolly", "joyful", "keen", "kind", "lively",
    "lovely", "lucky", "luminous", "magical", "majestic", "mellow", "merry",
    "mighty", "misty", "noble", "peaceful", "playful", "polished", "precious",
    "proud", "quiet", "quirky", "radiant", "rosy", "serene", "shiny", "silly",
    "sleepy", "smooth", "snazzy", "snug", "snuggly", "soft", "sparkling",
    "spicy", "splendid", "sprightly", "starry", "steady", "sunny", "swift",
    "tender", "tidy", "toasty", "tranquil", "twinkly", "valiant", "vast",
    "velvet", "vivid", "warm", "whimsical", "wild", "wise", "witty",
    "wondrous", "zany", "zesty", "zippy",
];

const NOUNS: &[&str] = &[
    "aurora", "avalanche", "blossom", "breeze", "brook", "bubble", "canyon",
    "cascade", "cloud", "clover", "comet", "coral", "cosmos", "creek",
    "crescent", "crystal", "dawn", "dewdrop", "dusk", "eclipse", "ember",
    "feather", "fern", "firefly", "flame", "flurry", "fog", "forest",
    "frost", "galaxy", "garden", "glacier", "glade", "grove", "harbor",
    "horizon", "island", "lagoon", "lake", "leaf", "lightning", "meadow",
    "meteor", "mist", "moon", "moonbeam", "mountain", "nebula", "nova",
    "ocean", "orbit", "pebble", "petal", "pine", "planet", "pond", "puddle",
    "quasar", "rain", "rainbow", "reef", "ripple", "river", "shore", "sky",
    "snowflake", "spark", "spring", "star", "stardust", "starlight",
    "storm", "stream", "summit", "sun", "sunbeam", "sunrise", "sunset",
    "thunder", "tide", "twilight", "valley", "volcano", "waterfall",
    "wave", "willow", "wind",
];

const VERBS: &[&str] = &[
    "baking", "beaming", "booping", "bouncing", "brewing", "bubbling",
    "chasing", "churning", "coalescing", "conjuring", "cooking", "crafting",
    "crunching", "cuddling", "dancing", "dazzling", "discovering",
    "doodling", "dreaming", "drifting", "enchanting", "exploring",
    "finding", "floating", "fluttering", "foraging", "forging",
    "frolicking", "gathering", "giggling", "gliding", "greeting",
    "growing", "hatching", "herding", "honking", "hopping", "hugging",
    "humming", "imagining", "inventing", "jingling", "juggling",
    "jumping", "kindling", "knitting", "launching", "leaping", "mapping",
    "marinating", "meandering", "mixing", "moseying", "munching",
    "napping", "nibbling", "noodling", "orbiting", "painting",
    "percolating", "petting", "plotting", "pondering", "popping",
    "prancing", "purring", "puzzling", "questing", "riding", "roaming",
    "rolling", "sauteeing", "scribbling", "seeking", "shimmying",
    "singing", "skipping", "sleeping", "snacking", "sniffing",
    "snuggling", "soaring", "sparking", "spinning", "splashing",
    "sprouting", "squishing", "stargazing", "stirring", "strolling",
    "swimming", "swinging", "tickling", "tinkering", "toasting",
    "tumbling", "twirling", "waddling", "wandering", "watching",
    "weaving", "whistling", "wibbling", "wiggling", "wishing",
    "wobbling", "wondering", "yawning", "zooming",
];

fn pick_random(arr: &[&str]) -> &str {
    let mut rng = rand::thread_rng();
    let idx = rng.gen_range(0..arr.len());
    arr[idx]
}

/// Generates a random word slug in the format "adjective-verb-noun"
pub fn generate_word_slug() -> String {
    format!(
        "{}-{}-{}",
        pick_random(ADJECTIVES),
        pick_random(VERBS),
        pick_random(NOUNS)
    )
}

/// Generates a shorter random word slug in the format "adjective-noun"
pub fn generate_short_word_slug() -> String {
    format!(
        "{}-{}",
        pick_random(ADJECTIVES),
        pick_random(NOUNS)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_djb2_hash() {
        let hash = djb2_hash("hello");
        assert_ne!(hash, 0);
        // Deterministic
        assert_eq!(djb2_hash("hello"), djb2_hash("hello"));
    }

    #[test]
    fn test_hash_content() {
        let h1 = hash_content("hello");
        let h2 = hash_content("hello");
        assert_eq!(h1, h2);
        assert_ne!(h1, hash_content("world"));
    }

    #[test]
    fn test_hash_pair() {
        let h1 = hash_pair("ts", "code");
        let h2 = hash_pair("tsc", "ode");
        assert_ne!(h1, h2);
    }

    #[test]
    fn test_compute_fingerprint() {
        let fp = compute_fingerprint("hello world test message", "1.0");
        assert_eq!(fp.len(), 3);
    }

    #[test]
    fn test_validate_uuid() {
        assert!(validate_uuid("550e8400-e29b-41d4-a716-446655440000").is_some());
        assert!(validate_uuid("invalid").is_none());
        assert!(validate_uuid("").is_none());
    }

    #[test]
    fn test_create_agent_id() {
        let id = create_agent_id("compact");
        assert!(id.starts_with("acompact-"));
        let id2 = create_agent_id("");
        assert!(id2.starts_with("a"));
    }

    #[test]
    fn test_tagged_id() {
        let id = to_tagged_id("test");
        assert!(id.starts_with("test_"));
        assert_eq!(parse_tagged_id(&id), Some("test"));
        assert!(get_tagged_id_counter(&id).is_some());
        assert!(validate_tagged_id(&id, "test"));
        assert!(!validate_tagged_id(&id, "other"));
    }

    #[test]
    fn test_generate_word_slug() {
        let slug = generate_word_slug();
        let parts: Vec<&str> = slug.split('-').collect();
        assert_eq!(parts.len(), 3);
    }

    #[test]
    fn test_generate_short_word_slug() {
        let slug = generate_short_word_slug();
        let parts: Vec<&str> = slug.split('-').collect();
        assert_eq!(parts.len(), 2);
    }
}
