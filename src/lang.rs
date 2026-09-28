//! Language codes for translation pairs: canonicalization (like `Intl.getCanonicalLocales`) and
//! English display names (like `Intl.DisplayNames(['en'], { type: 'language' })`).

/// (ISO 639-1, ISO 639-2/T, ISO 639-2/B or "", English name)
const LANGUAGES: &[(&str, &str, &str, &str)] = &[
    ("aa", "aar", "", "Afar"),
    ("ab", "abk", "", "Abkhazian"),
    ("ae", "ave", "", "Avestan"),
    ("af", "afr", "", "Afrikaans"),
    ("ak", "aka", "", "Akan"),
    ("am", "amh", "", "Amharic"),
    ("an", "arg", "", "Aragonese"),
    ("ar", "ara", "", "Arabic"),
    ("as", "asm", "", "Assamese"),
    ("av", "ava", "", "Avaric"),
    ("ay", "aym", "", "Aymara"),
    ("az", "aze", "", "Azerbaijani"),
    ("ba", "bak", "", "Bashkir"),
    ("be", "bel", "", "Belarusian"),
    ("bg", "bul", "", "Bulgarian"),
    ("bi", "bis", "", "Bislama"),
    ("bm", "bam", "", "Bambara"),
    ("bn", "ben", "", "Bangla"),
    ("bo", "bod", "tib", "Tibetan"),
    ("br", "bre", "", "Breton"),
    ("bs", "bos", "", "Bosnian"),
    ("ca", "cat", "", "Catalan"),
    ("ce", "che", "", "Chechen"),
    ("ch", "cha", "", "Chamorro"),
    ("co", "cos", "", "Corsican"),
    ("cr", "cre", "", "Cree"),
    ("cs", "ces", "cze", "Czech"),
    ("cu", "chu", "", "Church Slavic"),
    ("cv", "chv", "", "Chuvash"),
    ("cy", "cym", "wel", "Welsh"),
    ("da", "dan", "", "Danish"),
    ("de", "deu", "ger", "German"),
    ("dv", "div", "", "Divehi"),
    ("dz", "dzo", "", "Dzongkha"),
    ("ee", "ewe", "", "Ewe"),
    ("el", "ell", "gre", "Greek"),
    ("en", "eng", "", "English"),
    ("eo", "epo", "", "Esperanto"),
    ("es", "spa", "", "Spanish"),
    ("et", "est", "", "Estonian"),
    ("eu", "eus", "baq", "Basque"),
    ("fa", "fas", "per", "Persian"),
    ("ff", "ful", "", "Fula"),
    ("fi", "fin", "", "Finnish"),
    ("fj", "fij", "", "Fijian"),
    ("fo", "fao", "", "Faroese"),
    ("fr", "fra", "fre", "French"),
    ("fy", "fry", "", "Western Frisian"),
    ("ga", "gle", "", "Irish"),
    ("gd", "gla", "", "Scottish Gaelic"),
    ("gl", "glg", "", "Galician"),
    ("gn", "grn", "", "Guarani"),
    ("gu", "guj", "", "Gujarati"),
    ("gv", "glv", "", "Manx"),
    ("ha", "hau", "", "Hausa"),
    ("he", "heb", "", "Hebrew"),
    ("hi", "hin", "", "Hindi"),
    ("ho", "hmo", "", "Hiri Motu"),
    ("hr", "hrv", "", "Croatian"),
    ("ht", "hat", "", "Haitian Creole"),
    ("hu", "hun", "", "Hungarian"),
    ("hy", "hye", "arm", "Armenian"),
    ("hz", "her", "", "Herero"),
    ("ia", "ina", "", "Interlingua"),
    ("id", "ind", "", "Indonesian"),
    ("ie", "ile", "", "Interlingue"),
    ("ig", "ibo", "", "Igbo"),
    ("ii", "iii", "", "Sichuan Yi"),
    ("ik", "ipk", "", "Inupiaq"),
    ("io", "ido", "", "Ido"),
    ("is", "isl", "ice", "Icelandic"),
    ("it", "ita", "", "Italian"),
    ("iu", "iku", "", "Inuktitut"),
    ("ja", "jpn", "", "Japanese"),
    ("jv", "jav", "", "Javanese"),
    ("ka", "kat", "geo", "Georgian"),
    ("kg", "kon", "", "Kongo"),
    ("ki", "kik", "", "Kikuyu"),
    ("kj", "kua", "", "Kuanyama"),
    ("kk", "kaz", "", "Kazakh"),
    ("kl", "kal", "", "Kalaallisut"),
    ("km", "khm", "", "Khmer"),
    ("kn", "kan", "", "Kannada"),
    ("ko", "kor", "", "Korean"),
    ("kr", "kau", "", "Kanuri"),
    ("ks", "kas", "", "Kashmiri"),
    ("ku", "kur", "", "Kurdish"),
    ("kv", "kom", "", "Komi"),
    ("kw", "cor", "", "Cornish"),
    ("ky", "kir", "", "Kyrgyz"),
    ("la", "lat", "", "Latin"),
    ("lb", "ltz", "", "Luxembourgish"),
    ("lg", "lug", "", "Ganda"),
    ("li", "lim", "", "Limburgish"),
    ("ln", "lin", "", "Lingala"),
    ("lo", "lao", "", "Lao"),
    ("lt", "lit", "", "Lithuanian"),
    ("lu", "lub", "", "Luba-Katanga"),
    ("lv", "lav", "", "Latvian"),
    ("mg", "mlg", "", "Malagasy"),
    ("mh", "mah", "", "Marshallese"),
    ("mi", "mri", "mao", "Māori"),
    ("mk", "mkd", "mac", "Macedonian"),
    ("ml", "mal", "", "Malayalam"),
    ("mn", "mon", "", "Mongolian"),
    ("mr", "mar", "", "Marathi"),
    ("ms", "msa", "may", "Malay"),
    ("mt", "mlt", "", "Maltese"),
    ("my", "mya", "bur", "Burmese"),
    ("na", "nau", "", "Nauru"),
    ("nb", "nob", "", "Norwegian Bokmål"),
    ("nd", "nde", "", "North Ndebele"),
    ("ne", "nep", "", "Nepali"),
    ("ng", "ndo", "", "Ndonga"),
    ("nl", "nld", "dut", "Dutch"),
    ("nn", "nno", "", "Norwegian Nynorsk"),
    ("no", "nor", "", "Norwegian"),
    ("nr", "nbl", "", "South Ndebele"),
    ("nv", "nav", "", "Navajo"),
    ("ny", "nya", "", "Nyanja"),
    ("oc", "oci", "", "Occitan"),
    ("oj", "oji", "", "Ojibwa"),
    ("om", "orm", "", "Oromo"),
    ("or", "ori", "", "Odia"),
    ("os", "oss", "", "Ossetic"),
    ("pa", "pan", "", "Punjabi"),
    ("pi", "pli", "", "Pali"),
    ("pl", "pol", "", "Polish"),
    ("ps", "pus", "", "Pashto"),
    ("pt", "por", "", "Portuguese"),
    ("qu", "que", "", "Quechua"),
    ("rm", "roh", "", "Romansh"),
    ("rn", "run", "", "Rundi"),
    ("ro", "ron", "rum", "Romanian"),
    ("ru", "rus", "", "Russian"),
    ("rw", "kin", "", "Kinyarwanda"),
    ("sa", "san", "", "Sanskrit"),
    ("sc", "srd", "", "Sardinian"),
    ("sd", "snd", "", "Sindhi"),
    ("se", "sme", "", "Northern Sami"),
    ("sg", "sag", "", "Sango"),
    ("si", "sin", "", "Sinhala"),
    ("sk", "slk", "slo", "Slovak"),
    ("sl", "slv", "", "Slovenian"),
    ("sm", "smo", "", "Samoan"),
    ("sn", "sna", "", "Shona"),
    ("so", "som", "", "Somali"),
    ("sq", "sqi", "alb", "Albanian"),
    ("sr", "srp", "", "Serbian"),
    ("ss", "ssw", "", "Swati"),
    ("st", "sot", "", "Southern Sotho"),
    ("su", "sun", "", "Sundanese"),
    ("sv", "swe", "", "Swedish"),
    ("sw", "swa", "", "Swahili"),
    ("ta", "tam", "", "Tamil"),
    ("te", "tel", "", "Telugu"),
    ("tg", "tgk", "", "Tajik"),
    ("th", "tha", "", "Thai"),
    ("ti", "tir", "", "Tigrinya"),
    ("tk", "tuk", "", "Turkmen"),
    ("tl", "tgl", "", "Tagalog"),
    ("tn", "tsn", "", "Tswana"),
    ("to", "ton", "", "Tongan"),
    ("tr", "tur", "", "Turkish"),
    ("ts", "tso", "", "Tsonga"),
    ("tt", "tat", "", "Tatar"),
    ("tw", "twi", "", "Twi"),
    ("ty", "tah", "", "Tahitian"),
    ("ug", "uig", "", "Uyghur"),
    ("uk", "ukr", "", "Ukrainian"),
    ("ur", "urd", "", "Urdu"),
    ("uz", "uzb", "", "Uzbek"),
    ("ve", "ven", "", "Venda"),
    ("vi", "vie", "", "Vietnamese"),
    ("vo", "vol", "", "Volapük"),
    ("wa", "wln", "", "Walloon"),
    ("wo", "wol", "", "Wolof"),
    ("xh", "xho", "", "Xhosa"),
    ("yi", "yid", "", "Yiddish"),
    ("yo", "yor", "", "Yoruba"),
    ("za", "zha", "", "Zhuang"),
    ("zh", "zho", "chi", "Chinese"),
    ("zu", "zul", "", "Zulu"),
];

/// Languages without a two-letter code.
const EXTRA_NAMES: &[(&str, &str)] = &[
    ("ast", "Asturian"),
    ("ceb", "Cebuano"),
    ("fil", "Filipino"),
    ("gsw", "Swiss German"),
    ("haw", "Hawaiian"),
    ("hmn", "Hmong"),
    ("nds", "Low German"),
    ("sco", "Scots"),
    ("yue", "Cantonese"),
];

/// Deprecated codes that canonicalize to another one.
const ALIASES: &[(&str, &str)] = &[
    ("iw", "he"),
    ("in", "id"),
    ("ji", "yi"),
    ("jw", "jv"),
    ("mo", "ro"),
    ("sh", "sr"),
];

/// Canonical primary language subtag of a BCP 47 tag ("FR" -> "fr", "fra" -> "fr", "en-US" -> "en"),
/// or None when the tag isn't well-formed.
pub fn canonical_language(code: &str) -> Option<String> {
    let code = code.trim();
    if code.is_empty() {
        return None;
    }
    let mut subtags = code.split('-');
    let primary = subtags.next()?;
    let primary_ok =
        primary.chars().all(|c| c.is_ascii_alphabetic()) && matches!(primary.len(), 2 | 3 | 5..=8);
    if !primary_ok {
        return None;
    }
    for sub in subtags {
        if sub.is_empty() || sub.len() > 8 || !sub.chars().all(|c| c.is_ascii_alphanumeric()) {
            return None;
        }
    }
    let lower = primary.to_ascii_lowercase();
    if let Some((_, to)) = ALIASES.iter().find(|(from, _)| *from == lower) {
        return Some(to.to_string());
    }
    if lower.len() == 3
        && let Some((two, ..)) = LANGUAGES
            .iter()
            .find(|(_, t, b, _)| *t == lower || (!b.is_empty() && *b == lower))
    {
        return Some(two.to_string());
    }
    Some(lower)
}

/// English name of a canonical code ("fr" -> "French"), or the code itself when unknown.
pub fn language_name(code: &str) -> String {
    LANGUAGES
        .iter()
        .find(|(two, ..)| *two == code)
        .map(|(.., name)| name.to_string())
        .or_else(|| {
            EXTRA_NAMES
                .iter()
                .find(|(c, _)| *c == code)
                .map(|(_, n)| n.to_string())
        })
        .unwrap_or_else(|| code.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonicalizes() {
        assert_eq!(canonical_language("FR").as_deref(), Some("fr"));
        assert_eq!(canonical_language("fra").as_deref(), Some("fr"));
        assert_eq!(canonical_language("ger").as_deref(), Some("de"));
        assert_eq!(canonical_language("en-US").as_deref(), Some("en"));
        assert_eq!(canonical_language("not a language"), None);
        assert_eq!(language_name("fr"), "French");
        assert_eq!(language_name("en"), "English");
    }
}
