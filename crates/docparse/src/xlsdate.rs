//! 엑셀 날짜 셀을 사람이 읽는 글자로 — xlsx·xls 가 함께 쓴다.
//!
//! [왜 필요한가] 엑셀은 날짜를 **숫자**로 저장한다. 2012-12-31 은 파일 안에
//! `41274` 로 들어 있고, "날짜처럼 보여라"는 **서식**이 따로 붙어 있을 뿐이다.
//! 서식을 읽지 않으면 본문에 `41274` 가 그대로 나온다 — 사람이 표에서 보는
//! 글자와 다르다. 사이냅은 `2012-12-31` 로 바꿔 내놓는다(실측에서 확인).
//!
//! [왜 등급에 영향이 있나] 날짜 자체는 등급 신호가 아니지만, "계약일
//! 2012-12-31" 처럼 **날짜가 앵커와 붙어야 성립하는 규칙**은 일련번호로는
//! 걸리지 않는다. 또 일련번호는 다섯 자리 숫자라 다른 규칙의 헛detection 을
//! 부를 수도 있다.
//!
//! 서식 번호(numFmtId)는 xlsx 와 xls(BIFF)가 같은 체계를 쓴다 — 그래서 한 곳에
//! 두고 둘 다 부른다.

use chrono::{Duration, NaiveDate};

//------------------------------------------------------------------
// 서식이 무엇을 보여 주는 서식인가
//=> 날짜만·시각만·둘 다를 나눈다. 무엇을 붙여 쓸지가 달라진다.
//
// -필드: None     = 날짜·시각과 무관(숫자 그대로 둔다)
// -필드: Date     = 날짜만 (2012-12-31)
// -필드: Time     = 시각만 (13:34:00)
// -필드: DateTime = 둘 다 (2012-12-31 13:34:00)
//------------------------------------------------------------------
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DateKind {
    None,
    Date,
    Time,
    DateTime,
}

//------------------------------------------------------------------
// 내장 서식 번호가 날짜/시각인가
//=> 엑셀에는 번호만으로 뜻이 정해진 '내장 서식'이 있다(0~163). 이 번호는
//   파일 안에 서식 문자열이 없으므로 표를 보고 판단해야 한다.
//   xlsx 와 xls 가 같은 번호 체계를 쓴다.
//
//   14~17 날짜 · 18~21 시각 · 22 날짜+시각 · 45~47 경과시간(시각)
//   27~36, 50~58 은 한중일 지역 날짜 서식이다(연/월/일이 한자·한글로 섞여 온다).
//
// -in: id = 서식 번호(numFmtId · BIFF 의 ifmt)
//
// -out: 서식의 종류
// -out: error = 없음(모르는 번호는 None)
//------------------------------------------------------------------
pub fn builtin_kind(id: u16) -> DateKind {
    match id {
        14..=17 => DateKind::Date,
        18..=21 => DateKind::Time,
        22 => DateKind::DateTime,
        // 한중일 지역 날짜 서식 묶음.
        27..=36 | 50..=58 => DateKind::Date,
        // 45 mm:ss · 46 [h]:mm:ss · 47 mmss.0 — 모두 시각 쪽이다.
        45..=47 => DateKind::Time,
        _ => DateKind::None,
    }
}

//------------------------------------------------------------------
// 사용자 지정 서식 문자열이 날짜/시각인가
//=> 사용자가 만든 서식(번호 164 이상)은 "yyyy-mm-dd" 같은 **문자열**로 온다.
//   글자를 훑어 날짜 기호(y·d)와 시각 기호(h·s)가 있는지 본다.
//    1) 세미콜론 앞 첫 구역만 본다 — 뒤는 음수·0·문자열용이라 양수 표시가 기준이다
//    2) 따옴표 안 글자와 [빨강] 같은 색·조건 구역은 서식 기호가 아니라 건너뛴다
//    3) [h] [m] [s] 는 '경과 시간' 기호라 시각으로 센다
//
//   [왜 m 만으로는 날짜라 하지 않나] m 은 '월'도 되고 '분'도 된다(mm:ss 의 mm 은
//   분이다). 혼자 있으면 가릴 수 없어, y 나 d 가 함께 있을 때만 날짜로 본다.
//   잘못 날짜로 보면 멀쩡한 숫자가 엉뚱한 날짜 글자로 바뀐다 — 그 쪽이 더 나쁘다.
//
// -in: code = 서식 문자열(예: "yyyy\"년\" m\"월\"")
//
// -out: 서식의 종류
// -out: error = 없음(판단이 안 서면 None)
//------------------------------------------------------------------
pub fn code_kind(code: &str) -> DateKind {
    // 양수 구역만 본다.
    let head = code.split(';').next().unwrap_or("");
    let mut has_date = false;
    let mut has_time = false;
    let mut chars = head.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            // "..." 안은 그대로 찍히는 글자다 — 서식 기호가 아니다.
            '"' => {
                for c in chars.by_ref() {
                    if c == '"' { break; }
                }
            }
            // \x 는 바로 뒤 한 글자를 글자 그대로 쓰라는 뜻이다.
            '\\' => { chars.next(); }
            // [빨강]·[>100] 같은 구역은 건너뛴다. 다만 [h]·[m]·[s] 는 경과 시간이다.
            '[' => {
                let mut inner = String::new();
                for c in chars.by_ref() {
                    if c == ']' { break; }
                    inner.push(c);
                }
                let t = inner.trim().to_ascii_lowercase();
                if t.chars().all(|c| c == 'h' || c == 'm' || c == 's') && !t.is_empty() {
                    has_time = true;
                }
            }
            'y' | 'Y' | 'd' | 'D' => has_date = true,
            'h' | 'H' | 's' | 'S' => has_time = true,
            _ => {}
        }
    }
    match (has_date, has_time) {
        (true, true) => DateKind::DateTime,
        (true, false) => DateKind::Date,
        (false, true) => DateKind::Time,
        (false, false) => DateKind::None,
    }
}

//------------------------------------------------------------------
// 엑셀 일련번호를 날짜 글자로
//=> 엑셀은 1900-01-01 을 1 로 두고 하루에 1씩 센다. 소수 부분은 하루 중 시각이다.
//    1) 정수 부분으로 날짜를, 소수 부분으로 시각을 만든다
//    2) 종류에 따라 날짜만·시각만·둘 다를 돌려준다
//
//   [1900년 2월 29일 문제] 엑셀은 1900년을 윤년으로 잘못 알고 있어, 실제로는
//   없는 1900-02-29 를 60번으로 세어 둔다. 그래서 60 이후는 하루가 밀려 있다.
//   61 이상은 기준일을 1899-12-30 으로, 59 이하는 1899-12-31 로 잡으면 두 구간이
//   모두 맞는다. 60 은 있지도 않은 날이라 그대로 적어 준다(엑셀 화면과 같게).
//
// -in: v    = 셀에 들어 있는 숫자(일련번호)
// -in: kind = 서식의 종류
//
// -out: 사람이 읽는 글자. 바꿀 수 없으면 None(=숫자를 그대로 두라는 뜻)
// -out: error = 없음(범위를 벗어나면 None)
//------------------------------------------------------------------
pub fn serial_to_string(v: f64, kind: DateKind) -> Option<String> {
    if kind == DateKind::None || !v.is_finite() || v < 0.0 {
        return None;
    }
    // 9999-12-31 이 2958465 다. 그보다 크면 날짜로 볼 수 없다.
    if v >= 2_958_466.0 {
        return None;
    }

    let days = v.floor() as i64;
    // 하루를 초로 나눈 나머지. 반올림해야 0.5 가 12:00:00 으로 떨어진다.
    let mut secs = ((v - v.floor()) * 86_400.0).round() as i64;
    let mut days = days;
    // 23:59:59.7 처럼 반올림이 하루를 넘기면 날짜를 하루 올린다.
    if secs >= 86_400 {
        secs -= 86_400;
        days += 1;
    }

    let hh = secs / 3600;
    let mm = (secs % 3600) / 60;
    let ss = secs % 60;
    let time = format!("{:02}:{:02}:{:02}", hh, mm, ss);

    if kind == DateKind::Time {
        return Some(time);
    }

    // 60 번은 엑셀에만 있는 가짜 날짜다 — 계산하지 말고 그대로 적는다.
    let date = if days == 60 {
        "1900-02-29".to_string()
    } else {
        let base = if days >= 61 {
            NaiveDate::from_ymd_opt(1899, 12, 30)?
        } else {
            NaiveDate::from_ymd_opt(1899, 12, 31)?
        };
        base.checked_add_signed(Duration::try_days(days)?)?
            .format("%Y-%m-%d")
            .to_string()
    };

    match kind {
        DateKind::Date => Some(date),
        DateKind::DateTime => Some(format!("{} {}", date, time)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 실측 대조 — 사이냅이 2012-12-31 로 내놓은 칸이 파일 안에서는 41274 였다.
    #[test]
    fn 실측_일련번호를_사이냅과_같은_날짜로() {
        assert_eq!(serial_to_string(41274.0, DateKind::Date).unwrap(), "2012-12-31");
        assert_eq!(serial_to_string(41275.0, DateKind::Date).unwrap(), "2013-01-01");
    }

    // 1900 윤년 착각 구간 — 60 앞뒤로 하루가 밀린다. 여기를 틀리면 옛 문서의
    // 날짜가 전부 하루씩 어긋난다.
    #[test]
    fn 천구백년_윤년_착각_구간을_맞춘다() {
        assert_eq!(serial_to_string(1.0, DateKind::Date).unwrap(), "1900-01-01");
        assert_eq!(serial_to_string(59.0, DateKind::Date).unwrap(), "1900-02-28");
        assert_eq!(serial_to_string(60.0, DateKind::Date).unwrap(), "1900-02-29");
        assert_eq!(serial_to_string(61.0, DateKind::Date).unwrap(), "1900-03-01");
    }

    #[test]
    fn 시각과_날짜시각을_함께_낸다() {
        assert_eq!(serial_to_string(0.5, DateKind::Time).unwrap(), "12:00:00");
        assert_eq!(serial_to_string(41274.5, DateKind::DateTime).unwrap(),
                   "2012-12-31 12:00:00");
    }

    // 서식이 날짜가 아니면 숫자를 건드리면 안 된다 — 멀쩡한 값이 날짜로 둔갑한다.
    #[test]
    fn 날짜서식이_아니면_바꾸지_않는다() {
        assert!(serial_to_string(41274.0, DateKind::None).is_none());
        assert!(serial_to_string(-1.0, DateKind::Date).is_none());
        assert!(serial_to_string(3_000_000.0, DateKind::Date).is_none());
    }

    #[test]
    fn 내장_서식_번호를_가른다() {
        assert_eq!(builtin_kind(14), DateKind::Date);
        assert_eq!(builtin_kind(22), DateKind::DateTime);
        assert_eq!(builtin_kind(20), DateKind::Time);
        assert_eq!(builtin_kind(31), DateKind::Date);
        // 0(일반) · 2(소수) · 9(백분율) 같은 숫자 서식은 건드리면 안 된다.
        for id in [0u16, 1, 2, 9, 10, 37, 40, 44, 49] {
            assert_eq!(builtin_kind(id), DateKind::None, "{id} 를 날짜로 봤다");
        }
    }

    // m 은 '월'도 '분'도 된다. 혼자 있으면 날짜라 단정하지 않는다 —
    // 단정하면 mm:ss 같은 서식에서 멀쩡한 숫자가 날짜로 둔갑한다.
    #[test]
    fn 사용자서식_문자열을_가른다() {
        assert_eq!(code_kind("yyyy-mm-dd"), DateKind::Date);
        assert_eq!(code_kind("yyyy\"년\" m\"월\" d\"일\""), DateKind::Date);
        assert_eq!(code_kind("yyyy-mm-dd hh:mm:ss"), DateKind::DateTime);
        assert_eq!(code_kind("[h]:mm:ss"), DateKind::Time);
        // mm:ss 는 '분:초'라 시각이다(내장 45 번과 같은 뜻). 여기의 m 을 '월'로
        // 읽었다면 Date 가 나왔을 것이다 — m 혼자로는 날짜라 하지 않는 이유다.
        assert_eq!(code_kind("mm:ss"), DateKind::Time);
        assert_eq!(code_kind("#,##0"), DateKind::None);
        // 월만 있는 서식은 날짜로 단정하지 않는다(분과 가릴 수 없다).
        assert_eq!(code_kind("mm"), DateKind::None);
        // 따옴표 안의 글자는 서식 기호가 아니다 — "day" 의 d 에 속으면 안 된다.
        assert_eq!(code_kind("#,##0\"day\""), DateKind::None);
        // 색 구역은 건너뛴다 — [Red] 의 d 에 속으면 안 된다.
        assert_eq!(code_kind("[Red]#,##0"), DateKind::None);
        // 음수 구역은 보지 않는다(양수 표시가 기준이다).
        assert_eq!(code_kind("#,##0;[Red]\\-yyyy"), DateKind::None);
    }
}
