use base64::Engine as _;
use std::ffi::{c_char, c_int, CStr, CString};
use std::sync::Mutex;

// Mirage weights are 21 MB, so embed them: no download step, no Android asset plumbing.
static WEIGHTS: &[u8] = include_bytes!("../../vendor/mirage.cact");
const OUT_CAP: usize = 4 << 20;

extern "C" {
    fn cactus_load(cact: *const u8, n: u64) -> c_int;
    fn cactus_models() -> c_int;
    fn cactus_last_error() -> *const c_char;
    fn cactus_describe(
        rgb: *const u8, width: c_int, height: c_int,
        options: *const c_char, language: *const c_char, keywords: *const c_char,
        out: *mut c_char, out_capacity: c_int,
    ) -> c_int;
}

fn last_error() -> String {
    unsafe { CStr::from_ptr(cactus_last_error()).to_string_lossy().into_owned() }
}

/// The engine is a global singleton; this mutex serializes calls and owns the output buffer.
struct Engine(Mutex<Vec<u8>>);

fn load() -> Result<(), String> {
    unsafe {
        if cactus_load(WEIGHTS.as_ptr(), WEIGHTS.len() as u64) < 0 {
            return Err(last_error());
        }
        if cactus_models() & 4 == 0 {
            return Err("embedded weights are not a Mirage vision model".into());
        }
    }
    Ok(())
}

/// Packed RGBA (straight from canvas getImageData) -> RGB.
fn rgba_to_rgb(rgba: &[u8]) -> Vec<u8> {
    rgba.chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]]).collect()
}

/// Body: RGBA bytes (raw, or a base64 string on Android). Headers: x-width, x-height, optional x-keywords (newline separated).
/// Returns Mirage's JSON: {scene, objects:[{id,name,bbox:[x1,y1,x2,y2] in 0..1000,score,mask,position,description}], relations, text, events, reasoning}.
#[tauri::command]
async fn detect(request: tauri::ipc::Request<'_>, engine: tauri::State<'_, Engine>) -> Result<String, String> {
    let decoded;
    let rgba: &[u8] = match request.body() {
        tauri::ipc::InvokeBody::Raw(b) => b,
        // Android IPC has no raw-bytes channel, so the UI sends base64 there.
        tauri::ipc::InvokeBody::Json(serde_json::Value::String(s)) => {
            decoded = base64::engine::general_purpose::STANDARD.decode(s).map_err(|e| e.to_string())?;
            &decoded
        }
        _ => return Err("expected RGBA bytes (raw or base64 string)".into()),
    };
    let header = |k: &str| request.headers().get(k).and_then(|v| v.to_str().ok()).unwrap_or("");
    let (w, h): (usize, usize) = (
        header("x-width").parse().map_err(|_| "bad x-width")?,
        header("x-height").parse().map_err(|_| "bad x-height")?,
    );
    if w == 0 || h == 0 || rgba.len() != w * h * 4 {
        return Err(format!("body is {} bytes, expected {w}x{h}x4", rgba.len()));
    }
    // Header values are ASCII-only, so the UI percent-encodes keywords.
    let keywords = percent_decode(header("x-keywords"));
    let keywords = (!keywords.trim().is_empty())
        .then(|| CString::new(keywords).map_err(|e| e.to_string()))
        .transpose()?;
    let rgb = rgba_to_rgb(rgba);
    // Default = all sections (scene, relations, OCR text, events, `reasoning` summary); x-describe: 0 = objects only.
    let options = if header("x-describe") == "0" { c"{\"sections\":[\"objects\"]}" } else { c"{}" };

    let mut out = engine.0.lock().map_err(|e| e.to_string())?;
    let t0 = std::time::Instant::now();
    let code = unsafe {
        cactus_describe(
            rgb.as_ptr(), w as c_int, h as c_int, options.as_ptr(), std::ptr::null(),
            keywords.as_ref().map_or(std::ptr::null(), |k| k.as_ptr()),
            out.as_mut_ptr() as *mut c_char, OUT_CAP as c_int,
        )
    };
    if code < 0 {
        return Err(last_error());
    }
    let json = unsafe { CStr::from_ptr(out.as_ptr() as *const c_char) }.to_string_lossy();
    Ok(with_model_ms(&json, t0.elapsed().as_millis()))
}

/// Adds "model_ms" to Mirage's top-level JSON object so the UI can split model time from transfer time.
fn with_model_ms(json: &str, ms: u128) -> String {
    match json.strip_prefix('{') {
        Some(rest) if rest.trim_start().starts_with('}') => format!("{{\"model_ms\":{ms}}}"),
        Some(rest) => format!("{{\"model_ms\":{ms},{rest}"),
        None => json.to_owned(),
    }
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |j: usize| b.get(j).and_then(|c| (*c as char).to_digit(16));
        match (b[i], hex(i + 1), hex(i + 2)) {
            (b'%', Some(hi), Some(lo)) => { out.push((hi * 16 + lo) as u8); i += 3; }
            (c, _, _) => { out.push(c); i += 1; }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    load().expect("failed to load Mirage");
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(Engine(Mutex::new(vec![0u8; OUT_CAP])))
        .invoke_handler(tauri::generate_handler![detect])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers() {
        assert_eq!(rgba_to_rgb(&[1, 2, 3, 255, 4, 5, 6, 0]), [1, 2, 3, 4, 5, 6]);
        assert_eq!(with_model_ms("{\"a\":1}", 5), "{\"model_ms\":5,\"a\":1}");
        assert_eq!(with_model_ms("{}", 5), "{\"model_ms\":5}");
        assert_eq!(percent_decode("cat%0Acaf%C3%A9%2"), "cat\ncafé%2");
    }

    #[test]
    fn detects_on_noise() {
        load().unwrap();
        let (w, h) = (64, 48);
        let rgb: Vec<u8> = (0..w * h * 3).map(|i| (i * 31 % 251) as u8).collect();
        let mut out = vec![0u8; OUT_CAP];
        let code = unsafe {
            cactus_describe(rgb.as_ptr(), w as c_int, h as c_int, c"{\"sections\":[\"objects\"]}".as_ptr(),
                std::ptr::null(), std::ptr::null(), out.as_mut_ptr() as *mut c_char, OUT_CAP as c_int)
        };
        assert!(code >= 0, "{}", last_error());
        let json = unsafe { CStr::from_ptr(out.as_ptr() as *const c_char) }.to_string_lossy();
        assert!(json.contains("\"objects\""), "{json}");
    }
}
