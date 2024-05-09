// std imports
use std::sync::Arc;

// local imports
use crate::{
    datefmt::DateTimeFormatter,
    filtering::IncludeExcludeSetting,
    fmtx::{aligned_left, centered, OptimizedBuf, Push},
    model::{self, Caller, Level, RawValue},
    settings::Formatting,
    theme::{Element, StylingPush, Theme},
    IncludeExcludeKeyFilter,
};
use encstr::EncodedString;

// relative imports
use string::{Format, MessageFormatAuto, ValueFormatAuto};

// ---

type Buf = Vec<u8>;

// ---

pub trait RecordWithSourceFormatter {
    fn format_record(&self, buf: &mut Buf, rec: model::RecordWithSource);
}

pub struct RawRecordFormatter {}

impl RecordWithSourceFormatter for RawRecordFormatter {
    #[inline(always)]
    fn format_record(&self, buf: &mut Buf, rec: model::RecordWithSource) {
        buf.extend_from_slice(rec.source);
    }
}

impl<T: RecordWithSourceFormatter> RecordWithSourceFormatter for &T {
    #[inline(always)]
    fn format_record(&self, buf: &mut Buf, rec: model::RecordWithSource) {
        (**self).format_record(buf, rec)
    }
}

impl RecordWithSourceFormatter for Box<dyn RecordWithSourceFormatter> {
    #[inline(always)]
    fn format_record(&self, buf: &mut Buf, rec: model::RecordWithSource) {
        (**self).format_record(buf, rec)
    }
}

// ---

pub struct RecordFormatter {
    theme: Arc<Theme>,
    unescape_fields: bool,
    ts_formatter: DateTimeFormatter,
    ts_width: usize,
    hide_empty_fields: bool,
    flatten: bool,
    always_show_time: bool,
    always_show_level: bool,
    fields: Arc<IncludeExcludeKeyFilter>,
    cfg: Formatting,
}

impl RecordFormatter {
    pub fn new(
        theme: Arc<Theme>,
        ts_formatter: DateTimeFormatter,
        hide_empty_fields: bool,
        fields: Arc<IncludeExcludeKeyFilter>,
        cfg: Formatting,
    ) -> Self {
        let ts_width = ts_formatter.max_length();
        RecordFormatter {
            theme,
            unescape_fields: true,
            ts_formatter,
            ts_width,
            hide_empty_fields,
            flatten: false,
            always_show_time: false,
            always_show_level: false,
            fields,
            cfg,
        }
    }

    pub fn with_field_unescaping(self, unescape_fields: bool) -> Self {
        Self {
            unescape_fields,
            ..self
        }
    }

    pub fn with_flatten(self, flatten: bool) -> Self {
        Self { flatten, ..self }
    }

    pub fn with_always_show_time(self, value: bool) -> Self {
        Self {
            always_show_time: value,
            ..self
        }
    }

    pub fn with_always_show_level(self, value: bool) -> Self {
        Self {
            always_show_level: value,
            ..self
        }
    }

    pub fn format_record(&self, buf: &mut Buf, rec: &model::Record) {
        let mut fs = FormattingState::new(self.flatten && self.unescape_fields);

        self.theme.apply(buf, &rec.level, |s| {
            //
            // time
            //
            if let Some(ts) = &rec.ts {
                fs.add_element(|| {});
                s.element(Element::Time, |s| {
                    s.batch(|buf| {
                        aligned_left(buf, self.ts_width, b' ', |mut buf| {
                            if ts
                                .as_rfc3339()
                                .and_then(|ts| self.ts_formatter.reformat_rfc3339(&mut buf, ts))
                                .is_none()
                            {
                                if let Some(ts) = ts.parse() {
                                    self.ts_formatter.format(&mut buf, ts);
                                } else {
                                    buf.extend_from_slice(ts.raw().as_bytes());
                                }
                            }
                        });
                    })
                });
            } else if self.always_show_time {
                fs.add_element(|| {});
                s.element(Element::Time, |s| {
                    s.batch(|buf| {
                        centered(buf, self.ts_width, b'-', |mut buf| {
                            buf.extend_from_slice(b"-");
                        });
                    })
                });
            }

            //
            // level
            //
            let level = match rec.level {
                Some(Level::Debug) => Some(b"DBG"),
                Some(Level::Info) => Some(b"INF"),
                Some(Level::Warning) => Some(b"WRN"),
                Some(Level::Error) => Some(b"ERR"),
                _ => None,
            };
            let level = level.or_else(|| self.always_show_level.then(|| b"(?)"));
            if let Some(level) = level {
                fs.add_element(|| s.space());
                s.element(Element::Level, |s| {
                    s.batch(|buf| {
                        buf.extend_from_slice(self.cfg.punctuation.level_left_separator.as_bytes());
                    });
                    s.element(Element::LevelInner, |s| s.batch(|buf| buf.extend_from_slice(level)));
                    s.batch(|buf| buf.extend_from_slice(self.cfg.punctuation.level_right_separator.as_bytes()));
                });
            }

            //
            // logger
            //
            if let Some(logger) = rec.logger {
                fs.add_element(|| s.batch(|buf| buf.push(b' ')));
                s.element(Element::Logger, |s| {
                    s.element(Element::LoggerInner, |s| {
                        s.batch(|buf| buf.extend_from_slice(logger.as_bytes()))
                    });
                    s.batch(|buf| buf.extend_from_slice(self.cfg.punctuation.logger_name_separator.as_bytes()));
                });
            }
            //
            // message text
            //
            if let Some(value) = &rec.message {
                self.format_message(s, &mut fs, *value);
            } else {
                s.reset();
            }
            //
            // fields
            //
            let mut some_fields_hidden = false;
            for (k, v) in rec.fields() {
                if !self.hide_empty_fields || !v.is_empty() {
                    some_fields_hidden |= !self.format_field(s, k, *v, &mut fs, Some(&self.fields));
                }
            }
            if some_fields_hidden {
                s.element(Element::Ellipsis, |s| {
                    s.batch(|buf| buf.extend_from_slice(self.cfg.punctuation.hidden_fields_indicator.as_bytes()))
                });
            }
            //
            // caller
            //
            if let Some(caller) = &rec.caller {
                s.element(Element::Caller, |s| {
                    s.batch(|buf| {
                        buf.push(b' ');
                        buf.extend_from_slice(self.cfg.punctuation.source_location_separator.as_bytes())
                    });
                    s.element(Element::CallerInner, |s| {
                        s.batch(|buf| {
                            match caller {
                                Caller::Text(text) => {
                                    buf.extend_from_slice(text.as_bytes());
                                }
                                Caller::FileLine(file, line) => {
                                    buf.extend_from_slice(file.as_bytes());
                                    if line.len() != 0 {
                                        buf.push(b':');
                                        buf.extend_from_slice(line.as_bytes());
                                    }
                                }
                            };
                        });
                    });
                });
            };
        });
    }

    #[inline]
    fn format_field<'a, S: StylingPush<Buf>>(
        &self,
        s: &mut S,
        key: &str,
        value: RawValue<'a>,
        fs: &mut FormattingState,
        filter: Option<&IncludeExcludeKeyFilter>,
    ) -> bool {
        let mut fv = FieldFormatter::new(self);
        fv.format(s, key, value, fs, filter, IncludeExcludeSetting::Unspecified)
    }

    #[inline]
    fn format_message<'a, S: StylingPush<Buf>>(&self, s: &mut S, fs: &mut FormattingState, value: RawValue<'a>) {
        match value {
            RawValue::String(value) => {
                if !value.is_empty() {
                    fs.add_element(|| {
                        s.reset();
                        s.space();
                    });
                    s.element(Element::Message, |s| {
                        s.batch(|buf| buf.with_auto_trim(|buf| MessageFormatAuto::new(value).format(buf).unwrap()))
                    });
                }
                false
            }
            _ => self.format_field(s, "msg", value, fs, Some(self.fields.as_ref())),
        };
    }

    #[cfg(test)]
    fn with_theme(self, theme: Arc<Theme>) -> Self {
        Self { theme, ..self }
    }
}

impl RecordWithSourceFormatter for RecordFormatter {
    #[inline]
    fn format_record(&self, buf: &mut Buf, rec: model::RecordWithSource) {
        RecordFormatter::format_record(self, buf, rec.record)
    }
}

// ---

struct FormattingState {
    key_prefix: KeyPrefix,
    flatten: bool,
    empty: bool,
}

impl FormattingState {
    #[inline]
    fn new(flatten: bool) -> Self {
        Self {
            key_prefix: KeyPrefix::default(),
            flatten,
            empty: true,
        }
    }

    fn add_element(&mut self, add_space: impl FnOnce()) {
        if self.empty {
            self.empty = false;
        } else {
            add_space();
        }
    }
}

// ---

#[derive(Default)]
struct KeyPrefix {
    value: OptimizedBuf<u8, 256>,
}

impl KeyPrefix {
    #[inline]
    fn len(&self) -> usize {
        self.value.len()
    }

    #[inline]
    fn format<B: Push<u8>>(&self, buf: &mut B) {
        buf.extend_from_slice(&self.value.head);
        buf.extend_from_slice(&self.value.tail);
    }

    #[inline]
    fn push(&mut self, key: &str) -> usize {
        let len = self.len();
        if len != 0 {
            self.value.push(b'.');
        }
        key.key_prettify(&mut self.value);
        self.len() - len
    }

    #[inline]
    fn pop(&mut self, n: usize) {
        if n != 0 {
            let len = self.len();
            if n >= len {
                self.value.clear();
            } else {
                self.value.truncate(len - n);
            }
        }
    }
}

// ---

struct FieldFormatter<'a> {
    rf: &'a RecordFormatter,
}

impl<'a> FieldFormatter<'a> {
    fn new(rf: &'a RecordFormatter) -> Self {
        Self { rf }
    }

    fn format<S: StylingPush<Buf>>(
        &mut self,
        s: &mut S,
        key: &str,
        value: RawValue<'a>,
        fs: &mut FormattingState,
        filter: Option<&IncludeExcludeKeyFilter>,
        setting: IncludeExcludeSetting,
    ) -> bool {
        let (filter, setting, leaf) = match filter {
            Some(filter) => {
                let setting = setting.apply(filter.setting());
                match filter.get(key) {
                    Some(filter) => (Some(filter), setting.apply(filter.setting()), filter.leaf()),
                    None => (None, setting, true),
                }
            }
            None => (None, setting, true),
        };
        if setting == IncludeExcludeSetting::Exclude && leaf {
            return false;
        }
        let ffv = self.begin(s, key, value, fs);
        if self.rf.unescape_fields {
            self.format_value(s, value, fs, filter, setting);
        } else {
            s.element(Element::String, |s| {
                s.batch(|buf| buf.extend(value.raw_str().as_bytes()))
            });
        }
        self.end(fs, ffv);
        true
    }

    fn format_value<S: StylingPush<Buf>>(
        &mut self,
        s: &mut S,
        value: RawValue<'a>,
        fs: &mut FormattingState,
        filter: Option<&IncludeExcludeKeyFilter>,
        setting: IncludeExcludeSetting,
    ) {
        let value = match value {
            RawValue::String(EncodedString::Raw(value)) => RawValue::auto(value.as_str()),
            _ => value,
        };
        match value {
            RawValue::String(value) => {
                s.element(Element::String, |s| {
                    s.batch(|buf| buf.with_auto_trim(|buf| ValueFormatAuto::new(value).format(buf).unwrap()))
                });
            }
            RawValue::Number(value) => {
                s.element(Element::Number, |s| s.batch(|buf| buf.extend(value.as_bytes())));
            }
            RawValue::Boolean(true) => {
                s.element(Element::Boolean, |s| s.batch(|buf| buf.extend(b"true")));
            }
            RawValue::Boolean(false) => {
                s.element(Element::Boolean, |s| s.batch(|buf| buf.extend(b"false")));
            }
            RawValue::Null => {
                s.element(Element::Null, |s| s.batch(|buf| buf.extend(b"null")));
            }
            RawValue::Object(value) => {
                let item = value.parse().unwrap();
                s.element(Element::Object, |s| {
                    if !fs.flatten {
                        s.batch(|buf| buf.push(b'{'));
                    }
                    let mut some_fields_hidden = false;
                    for (k, v) in item.fields.iter() {
                        some_fields_hidden |= !self.format(s, k, *v, fs, filter, setting);
                    }
                    if some_fields_hidden {
                        s.element(Element::Ellipsis, |s| {
                            s.batch(|buf| buf.extend(self.rf.cfg.punctuation.hidden_fields_indicator.as_bytes()))
                        });
                    }
                    if !fs.flatten {
                        s.batch(|buf| {
                            if item.fields.len() != 0 {
                                buf.push(b' ');
                            }
                            buf.push(b'}');
                        });
                    }
                });
            }
            RawValue::Array(value) => {
                s.element(Element::Array, |s| {
                    let item = value.parse::<32>().unwrap();
                    s.batch(|buf| buf.push(b'['));
                    let mut first = true;
                    for v in item.iter() {
                        if !first {
                            s.batch(|buf| buf.extend(self.rf.cfg.punctuation.array_separator.as_bytes()));
                        } else {
                            first = false;
                        }
                        self.format_value(s, *v, fs, None, IncludeExcludeSetting::Unspecified);
                    }
                    s.batch(|buf| buf.push(b']'));
                });
            }
        };
    }

    #[inline(always)]
    fn begin<S: StylingPush<Buf>>(
        &mut self,
        s: &mut S,
        key: &str,
        value: RawValue<'a>,
        fs: &mut FormattingState,
    ) -> FormattedFieldVariant {
        if fs.flatten && matches!(value, RawValue::Object(_)) {
            return FormattedFieldVariant::Flattened(fs.key_prefix.push(key));
        }

        let variant = FormattedFieldVariant::Normal { flatten: fs.flatten };

        fs.add_element(|| s.space());
        s.element(Element::Key, |s| {
            s.batch(|buf| {
                if fs.flatten {
                    fs.flatten = false;
                    if fs.key_prefix.len() != 0 {
                        fs.key_prefix.format(buf);
                        buf.push(b'.');
                    }
                }
                key.key_prettify(buf);
            });
        });
        s.element(Element::Field, |s| {
            s.batch(|buf| buf.extend(self.rf.cfg.punctuation.field_key_value_separator.as_bytes()));
        });

        variant
    }

    #[inline]
    fn end(&mut self, fs: &mut FormattingState, v: FormattedFieldVariant) {
        match v {
            FormattedFieldVariant::Normal { flatten } => {
                fs.flatten = flatten;
            }
            FormattedFieldVariant::Flattened(n) => {
                fs.key_prefix.pop(n);
            }
        }
    }
}

// ---

pub trait WithAutoTrim {
    fn with_auto_trim<F>(&mut self, f: F)
    where
        F: FnOnce(&mut Self);
}

impl WithAutoTrim for Vec<u8> {
    #[inline(always)]
    fn with_auto_trim<F>(&mut self, f: F)
    where
        F: FnOnce(&mut Self),
    {
        let begin = self.len();
        f(self);
        if let Some(end) = self[begin..].iter().rposition(|&b| !b.is_ascii_whitespace()) {
            self.truncate(begin + end + 1);
        }
    }
}

// ---

trait KeyPrettify {
    fn key_prettify<B: Push<u8>>(&self, buf: &mut B);
}

impl KeyPrettify for str {
    #[inline]
    fn key_prettify<B: Push<u8>>(&self, buf: &mut B) {
        let bytes = self.as_bytes();
        let mut i = 0;
        while let Some(pos) = bytes[i..].iter().position(|&b| b == b'_') {
            buf.extend_from_slice(&bytes[i..i + pos]);
            buf.push(b'-');
            i += pos + 1;
        }
        buf.extend_from_slice(&bytes[i..])
    }
}

// ---

enum FormattedFieldVariant {
    Normal { flatten: bool },
    Flattened(usize),
}

// ---

pub mod string {
    // workspace imports
    use encstr::{AnyEncodedString, JsonAppender, Result};

    // third-party imports
    use bitmask_enum::bitmask;

    // ---

    pub trait Format {
        fn format(&self, buf: &mut Vec<u8>) -> Result<()>;
    }

    // ---

    pub struct ValueFormatAuto<S> {
        string: S,
    }

    impl<S> ValueFormatAuto<S> {
        #[inline(always)]
        pub fn new(string: S) -> Self {
            Self { string }
        }
    }

    impl<'a, S> Format for ValueFormatAuto<S>
    where
        S: AnyEncodedString<'a> + Clone + Copy,
    {
        #[inline(always)]
        fn format(&self, buf: &mut Vec<u8>) -> Result<()> {
            if self.string.is_empty() {
                buf.extend(r#""""#.as_bytes());
                return Ok(());
            }

            let begin = buf.len();
            ValueFormatRaw::new(self.string).format(buf)?;

            let mut mask = Mask::none();

            buf[begin..].iter().map(|&c| CHAR_GROUPS[c as usize]).for_each(|group| {
                mask |= group;
            });

            let first = buf[begin];
            if mask.is_none() && first != b'[' && first != b'{' {
                return Ok(());
            }

            if !mask.intersects(Mask::DoubleQuote | Mask::Control | Mask::Backslash) {
                buf.push(b'"');
                buf.push(b'"');
                buf[begin..].rotate_right(1);
                return Ok(());
            }

            if !mask.intersects(Mask::SingleQuote | Mask::Control | Mask::Backslash) {
                buf.push(b'\'');
                buf.push(b'\'');
                buf[begin..].rotate_right(1);
                return Ok(());
            }

            const Z: Mask = Mask::none();
            const XS: Mask = Mask::Control.or(Mask::ExtendedSpace);

            if matches!(mask.and(Mask::Backtick.or(XS)), Z | XS) {
                buf.push(b'`');
                buf.push(b'`');
                buf[begin..].rotate_right(1);
                return Ok(());
            }

            buf.truncate(begin);
            ValueFormatDoubleQuoted::new(self.string).format(buf)
        }
    }

    // ---

    pub struct ValueFormatRaw<S> {
        string: S,
    }

    impl<S> ValueFormatRaw<S> {
        #[inline(always)]
        pub fn new(string: S) -> Self {
            Self { string }
        }
    }

    impl<'a, S> Format for ValueFormatRaw<S>
    where
        S: AnyEncodedString<'a>,
    {
        #[inline(always)]
        fn format(&self, buf: &mut Vec<u8>) -> Result<()> {
            self.string.decode(buf)
        }
    }

    // ---

    pub struct ValueFormatDoubleQuoted<S> {
        string: S,
    }

    impl<S> ValueFormatDoubleQuoted<S> {
        #[inline(always)]
        pub fn new(string: S) -> Self {
            Self { string }
        }
    }

    impl<'a, S> Format for ValueFormatDoubleQuoted<S>
    where
        S: AnyEncodedString<'a>,
    {
        #[inline]
        fn format(&self, buf: &mut Vec<u8>) -> Result<()> {
            self.string.format_json(buf)
        }
    }

    // ---

    pub struct MessageFormatAuto<S> {
        string: S,
    }

    impl<S> MessageFormatAuto<S> {
        #[inline(always)]
        pub fn new(string: S) -> Self {
            Self { string }
        }
    }

    impl<'a, S> Format for MessageFormatAuto<S>
    where
        S: AnyEncodedString<'a> + Clone + Copy,
    {
        #[inline(always)]
        fn format(&self, buf: &mut Vec<u8>) -> Result<()> {
            if self.string.is_empty() {
                return Ok(());
            }

            let begin = buf.len();
            MessageFormatRaw::new(self.string).format(buf)?;
            if buf[begin..].starts_with(b"\"") || buf[begin..].contains(&b'=') {
                buf.truncate(begin);
                MessageFormatDoubleQuoted::new(self.string).format(buf)?;
            }
            Ok(())
        }
    }

    // ---

    pub struct MessageFormatRaw<S> {
        string: S,
    }

    impl<S> MessageFormatRaw<S> {
        #[inline(always)]
        pub fn new(string: S) -> Self {
            Self { string }
        }
    }

    impl<'a, S> Format for MessageFormatRaw<S>
    where
        S: AnyEncodedString<'a>,
    {
        #[inline(always)]
        fn format(&self, buf: &mut Vec<u8>) -> Result<()> {
            self.string.decode(buf)
        }
    }

    // ---

    pub struct MessageFormatDoubleQuoted<S> {
        string: S,
    }

    impl<S> MessageFormatDoubleQuoted<S> {
        #[inline(always)]
        pub fn new(string: S) -> Self {
            Self { string }
        }
    }

    impl<'a, S> Format for MessageFormatDoubleQuoted<S>
    where
        S: AnyEncodedString<'a>,
    {
        #[inline]
        fn format(&self, buf: &mut Vec<u8>) -> Result<()> {
            self.string.format_json(buf)
        }
    }

    // ---

    trait EncodedStringExt {
        fn format_json(&self, buf: &mut Vec<u8>) -> Result<()>;
    }

    impl<'a, S> EncodedStringExt for S
    where
        S: AnyEncodedString<'a>,
    {
        #[inline]
        fn format_json(&self, buf: &mut Vec<u8>) -> Result<()> {
            buf.push(b'"');
            self.decode(JsonAppender::new(buf))?;
            buf.push(b'"');
            Ok(())
        }
    }

    // ---

    static CHAR_GROUPS: [Mask; 256] = {
        const CT: Mask = Mask::Control; // 0x00..0x1F
        const DQ: Mask = Mask::DoubleQuote; // 0x22
        const SQ: Mask = Mask::SingleQuote; // 0x27
        const BS: Mask = Mask::Backslash; // 0x5C
        const BT: Mask = Mask::Backtick; // 0x60
        const SP: Mask = Mask::Space; // 0x20
        const XS: Mask = Mask::Control.or(Mask::ExtendedSpace); // 0x09, 0x0A, 0x0D
        const EQ: Mask = Mask::EqualSign; // 0x3D
        const __: Mask = Mask::none();
        [
            //   1   2   3   4   5   6   7   8   9   A   B   C   D   E   F
            CT, CT, CT, CT, CT, CT, CT, CT, CT, XS, XS, CT, CT, XS, CT, CT, // 0
            CT, CT, CT, CT, CT, CT, CT, CT, CT, CT, CT, CT, CT, CT, CT, CT, // 1
            SP, __, DQ, __, __, __, __, SQ, __, __, __, __, __, __, __, __, // 2
            __, __, __, __, __, __, __, __, __, __, __, __, __, EQ, __, __, // 3
            __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, // 4
            __, __, __, __, __, __, __, __, __, __, __, __, BS, __, __, __, // 5
            BT, __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, // 6
            __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, // 7
            __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, // 8
            __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, // 9
            __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, // A
            __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, // B
            __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, // C
            __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, // D
            __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, // E
            __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, // F
        ]
    };

    #[bitmask(u8)]
    enum Mask {
        Control,
        DoubleQuote,
        SingleQuote,
        Backslash,
        Backtick,
        Space,
        ExtendedSpace,
        EqualSign,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        datefmt::LinuxDateFormat,
        model::{RawObject, Record, RecordFields},
        settings::Punctuation,
        theme::Theme,
        themecfg::testing,
        timestamp::Timestamp,
        timezone::Tz,
    };
    use chrono::{Offset, Utc};
    use encstr::EncodedString;
    use serde_json as json;

    trait FormatToVec {
        fn format_to_vec(&self, rec: &Record) -> Vec<u8>;
    }

    trait FormatToString {
        fn format_to_string(&self, rec: &Record) -> String;
    }

    impl FormatToVec for RecordFormatter {
        fn format_to_vec(&self, rec: &Record) -> Vec<u8> {
            let mut buf = Vec::new();
            self.format_record(&mut buf, rec);
            buf
        }
    }

    impl FormatToString for RecordFormatter {
        fn format_to_string(&self, rec: &Record) -> String {
            String::from_utf8(self.format_to_vec(rec)).unwrap()
        }
    }

    fn formatter() -> RecordFormatter {
        RecordFormatter::new(
            Arc::new(Theme::from(testing::theme().unwrap())),
            DateTimeFormatter::new(
                LinuxDateFormat::new("%y-%m-%d %T.%3N").compile(),
                Tz::FixedOffset(Utc.fix()),
            ),
            false,
            Arc::new(IncludeExcludeKeyFilter::default()),
            Formatting {
                punctuation: Punctuation::test_default(),
                flatten: None,
            },
        )
    }

    fn format(rec: &Record) -> String {
        formatter().format_to_string(rec)
    }

    fn format_no_color(rec: &Record) -> String {
        formatter().with_theme(Default::default()).format_to_string(rec)
    }

    fn json_raw_value(s: &str) -> Box<json::value::RawValue> {
        json::value::RawValue::from_string(s.into()).unwrap()
    }

    #[test]
    fn test_nested_objects() {
        let ka = json_raw_value(r#"{"va":{"kb":42,"kc":43}}"#);
        let rec = Record {
            ts: Some(Timestamp::new("2000-01-02T03:04:05.123Z")),
            message: Some(RawValue::String(EncodedString::json(r#""tm""#))),
            level: Some(Level::Debug),
            logger: Some("tl"),
            caller: Some(Caller::Text("tc")),
            fields: RecordFields {
                head: heapless::Vec::from_slice(&[("k_a", RawValue::from(RawObject::Json(&ka)))]).unwrap(),
                ..Default::default()
            },
            ..Default::default()
        };

        assert_eq!(
            &format(&rec),
            "\u{1b}[0;2;3m00-01-02 03:04:05.123 \u{1b}[0;36m|\u{1b}[0;95mDBG\u{1b}[0;36m|\u{1b}[0;2;3m \u{1b}[0;2;4mtl:\u{1b}[0m \u{1b}[0;1;39mtm \u{1b}[0;32mk-a\u{1b}[0;2m=\u{1b}[0;33m{ \u{1b}[0;32mva\u{1b}[0;2m=\u{1b}[0;33m{ \u{1b}[0;32mkb\u{1b}[0;2m=\u{1b}[0;94m42 \u{1b}[0;32mkc\u{1b}[0;2m=\u{1b}[0;94m43\u{1b}[0;33m } }\u{1b}[0;2;3m @ tc\u{1b}[0m",
        );

        assert_eq!(
            &formatter().with_flatten(true).format_to_string(&rec),
            "\u{1b}[0;2;3m00-01-02 03:04:05.123 \u{1b}[0;36m|\u{1b}[0;95mDBG\u{1b}[0;36m|\u{1b}[0;2;3m \u{1b}[0;2;4mtl:\u{1b}[0m \u{1b}[0;1;39mtm \u{1b}[0;32mk-a.va.kb\u{1b}[0;2m=\u{1b}[0;94m42 \u{1b}[0;32mk-a.va.kc\u{1b}[0;2m=\u{1b}[0;94m43\u{1b}[0;2;3m @ tc\u{1b}[0m",
        );
    }

    #[test]
    fn test_timestamp_none() {
        let rec = Record {
            message: Some(RawValue::String(EncodedString::json(r#""tm""#))),
            level: Some(Level::Error),
            ..Default::default()
        };

        assert_eq!(&format(&rec), "\u{1b}[0;7;91m|ERR|\u{1b}[0m \u{1b}[0;1;39mtm\u{1b}[0m");
    }

    #[test]
    fn test_timestamp_none_always_show() {
        let rec = Record {
            message: Some(RawValue::String(EncodedString::json(r#""tm""#))),
            ..Default::default()
        };

        assert_eq!(
            &formatter().with_always_show_time(true).format_to_string(&rec),
            "\u{1b}[0;2;3m---------------------\u{1b}[0m \u{1b}[0;1;39mtm\u{1b}[0m",
        );
    }

    #[test]
    fn test_level_none() {
        let rec = Record {
            message: Some(RawValue::String(EncodedString::json(r#""tm""#))),
            ..Default::default()
        };

        assert_eq!(&format(&rec), "\u{1b}[0;1;39mtm\u{1b}[0m",);
    }

    #[test]
    fn test_level_none_always_show() {
        let rec = Record {
            message: Some(RawValue::String(EncodedString::json(r#""tm""#))),
            ..Default::default()
        };

        assert_eq!(
            &formatter().with_always_show_level(true).format_to_string(&rec),
            "\u{1b}[0;36m|(?)|\u{1b}[0m \u{1b}[0;1;39mtm\u{1b}[0m"
        );
    }

    #[test]
    fn test_string_value_raw() {
        let rec = Record {
            fields: RecordFields {
                head: heapless::Vec::from_slice(&[("k", RawValue::String(EncodedString::raw("v")))]).unwrap(),
                ..Default::default()
            },
            ..Default::default()
        };

        assert_eq!(&format_no_color(&rec), "k=v");
    }

    #[test]
    fn test_string_value_json_simple() {
        let rec = Record {
            fields: RecordFields {
                head: heapless::Vec::from_slice(&[("k", RawValue::String(EncodedString::json(r#""some-value""#)))])
                    .unwrap(),
                ..Default::default()
            },
            ..Default::default()
        };

        assert_eq!(&format_no_color(&rec), r#"k=some-value"#);
    }

    #[test]
    fn test_string_value_json_with_space() {
        let rec = Record {
            fields: RecordFields {
                head: heapless::Vec::from_slice(&[("k", RawValue::String(EncodedString::json(r#""some value""#)))])
                    .unwrap(),
                ..Default::default()
            },
            ..Default::default()
        };

        assert_eq!(&format_no_color(&rec), r#"k="some value""#);
    }

    #[test]
    fn test_string_value_json_with_space_and_double_quotes() {
        let rec = Record {
            fields: RecordFields {
                head: heapless::Vec::from_slice(&[("k", RawValue::String(EncodedString::json(r#""some \"value\"""#)))])
                    .unwrap(),
                ..Default::default()
            },
            ..Default::default()
        };

        assert_eq!(&format_no_color(&rec), r#"k='some "value"'"#);
    }

    #[test]
    fn test_string_value_json_with_space_and_single_quotes() {
        let rec = Record {
            fields: RecordFields {
                head: heapless::Vec::from_slice(&[("k", RawValue::String(EncodedString::json(r#""some 'value'""#)))])
                    .unwrap(),
                ..Default::default()
            },
            ..Default::default()
        };

        assert_eq!(&format_no_color(&rec), r#"k="some 'value'""#);
    }

    #[test]
    fn test_string_value_json_with_space_and_backticks() {
        let rec = Record {
            fields: RecordFields {
                head: heapless::Vec::from_slice(&[("k", RawValue::String(EncodedString::json(r#""some `value`""#)))])
                    .unwrap(),
                ..Default::default()
            },
            ..Default::default()
        };

        assert_eq!(&format_no_color(&rec), r#"k="some `value`""#);
    }

    #[test]
    fn test_string_value_json_with_space_and_double_and_single_quotes() {
        let rec = Record {
            fields: RecordFields {
                head: heapless::Vec::from_slice(&[(
                    "k",
                    RawValue::String(EncodedString::json(r#""some \"value\" from 'source'""#)),
                )])
                .unwrap(),
                ..Default::default()
            },
            ..Default::default()
        };

        assert_eq!(&format_no_color(&rec), r#"k=`some "value" from 'source'`"#);
    }

    #[test]
    fn test_string_value_json_with_backslash() {
        let rec = Record {
            fields: RecordFields {
                head: heapless::Vec::from_slice(&[(
                    "k",
                    RawValue::String(EncodedString::json(r#""some-\\\"value\\\"""#)),
                )])
                .unwrap(),
                ..Default::default()
            },
            ..Default::default()
        };

        assert_eq!(&format_no_color(&rec), r#"k=`some-\"value\"`"#);
    }

    #[test]
    fn test_string_value_json_with_space_and_double_and_single_quotes_and_backticks() {
        let rec = Record {
            fields: RecordFields {
                head: heapless::Vec::from_slice(&[(
                    "k",
                    RawValue::String(EncodedString::json(r#""some \"value\" from 'source' with `sauce`""#)),
                )])
                .unwrap(),
                tail: Default::default(),
            },
            ..Default::default()
        };

        assert_eq!(
            &format_no_color(&rec),
            r#"k="some \"value\" from 'source' with `sauce`""#
        );
    }

    #[test]
    fn test_string_value_json_with_extended_space() {
        let rec = Record {
            fields: RecordFields {
                head: heapless::Vec::from_slice(&[("k", RawValue::String(EncodedString::json(r#""some\tvalue""#)))])
                    .unwrap(),
                tail: Default::default(),
            },
            ..Default::default()
        };

        assert_eq!(&format_no_color(&rec), "k=`some\tvalue`");
    }

    #[test]
    fn test_string_value_json_with_control_characters() {
        let rec = Record {
            fields: RecordFields {
                head: heapless::Vec::from_slice(&[(
                    "k",
                    RawValue::String(EncodedString::json(r#""some-\u001b[1mvalue\u001b[0m""#)),
                )])
                .unwrap(),
                tail: Default::default(),
            },
            ..Default::default()
        };

        assert_eq!(&format_no_color(&rec), r#"k="some-\u001b[1mvalue\u001b[0m""#);
    }

    #[test]
    fn test_string_value_json_with_control_characters_and_quotes() {
        let rec = Record {
            fields: RecordFields {
                head: heapless::Vec::from_slice(&[(
                    "k",
                    RawValue::String(EncodedString::json(r#""some-\u001b[1m\"value\"\u001b[0m""#)),
                )])
                .unwrap(),
                tail: Default::default(),
            },
            ..Default::default()
        };

        assert_eq!(&format_no_color(&rec), r#"k="some-\u001b[1m\"value\"\u001b[0m""#);
    }

    #[test]
    fn test_message_double_quoted() {
        let rec = Record {
            message: Some(RawValue::String(EncodedString::raw(r#""hello, world""#))),
            ..Default::default()
        };

        let result = format_no_color(&rec);
        assert_eq!(&result, r#""\"hello, world\"""#, "{}", result);
    }
}
