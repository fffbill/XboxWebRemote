use base64::Engine;
use p256::ecdsa::{signature::Signer, Signature, SigningKey};
use p256::elliptic_curve::sec1::ToSec1Point;
use p256::pkcs8::{DecodePrivateKey, EncodePrivateKey, LineEnding};

pub fn b64url(data: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(data)
}

pub fn b64std(data: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(data)
}

pub fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// 宽容加载签名私钥: PKCS8 ("BEGIN PRIVATE KEY") 或 SEC1 ("BEGIN EC PRIVATE KEY", openssl 老格式)
fn load_signing_key(pem: &str) -> Result<SigningKey, String> {
    if let Ok(k) = SigningKey::from_pkcs8_pem(pem) {
        return Ok(k);
    }
    let secret = p256::SecretKey::from_sec1_pem(pem)
        .map_err(|e| format!("密钥解析失败: {:?}", e))?;
    Ok(SigningKey::from(secret))
}

/// SEC1/任意可解析 PEM → PKCS8 (迁移到标准格式)
fn to_pkcs8(pem: &str) -> Option<String> {
    let k = load_signing_key(pem).ok()?;
    let out = k.to_pkcs8_pem(LineEnding::LF).ok()?;
    Some(out.to_string())
}

/// 确保 App 数据里存在可用的 P-256 密钥:
/// - 无密钥 → 生成新密钥
/// - 密钥为 SEC1 老格式 → 迁移为 PKCS8 (保持身份不变)
/// - 密钥损坏 → 重新生成 (自愈)
pub fn ensure_keys(app: &crate::App) -> crate::state::Keys {
    let existing = {
        let p = app.persist.lock().unwrap();
        p.xalkeys.clone()
    };
    if let Some(k) = existing {
        if let Some(pkcs8) = to_pkcs8(&k.pem) {
            if pkcs8 == k.pem {
                return k;
            }
            let (x, y) = pubkey_xy(&pkcs8);
            let nk = crate::state::Keys { pem: pkcs8, x, y };
            {
                let mut p = app.persist.lock().unwrap();
                p.xalkeys = Some(nk.clone());
            }
            {
                let p = app.persist.lock().unwrap();
                crate::xal::save_tokens(app, &p);
            }
            return nk;
        }
        // 加载失败 → 走重新生成
    }
    let k = generate_keys();
    {
        let mut p = app.persist.lock().unwrap();
        p.xalkeys = Some(k.clone());
    }
    {
        let p = app.persist.lock().unwrap();
        crate::xal::save_tokens(app, &p);
    }
    k
}

/// 生成 P-256 密钥 (PKCS8 PEM), 返回 Keys (与 Python 版格式兼容)
pub fn generate_keys() -> crate::state::Keys {
    let sk = p256::SecretKey::random(&mut rand::rng());
    let pem = sk
        .to_pkcs8_pem(LineEnding::LF)
        .expect("pem encode")
        .to_string();
    let (x, y) = pubkey_xy(&pem);
    crate::state::Keys { pem, x, y }
}

/// PEM -> 公钥 (x, y) urlsafe-b64
pub fn pubkey_xy(pem: &str) -> (String, String) {
    let sk = load_signing_key(pem).expect("key load");
    let point = sk.verifying_key().to_sec1_point(false);
    let b = point.as_bytes();
    (b64url(&b[1..33]), b64url(&b[33..65]))
}

/// ECDSA-SHA256 签名 -> 64 字节 r||s (IEEE-P1363)
pub fn sign_p1363(pem: &str, data: &[u8]) -> Vec<u8> {
    match load_signing_key(pem) {
        Ok(sk) => {
            let sig: Signature = sk.sign(data);
            sig.to_bytes().to_vec()
        }
        Err(_) => vec![0u8; 64], // 不可达 (ensure_keys 已验证), 兜底防 panic
    }
}

/// Xbox ProofOfPossession 签名 (xal-node sign() 的移植)
pub fn xal_sign(keys: &crate::state::Keys, path: &str, payload: &str) -> String {
    let filetime = ((now() as u64) + 11644473600) * 10_000_000u64;
    let mut buf: Vec<u8> = Vec::new();
    buf.extend_from_slice(&1u32.to_be_bytes());
    buf.push(0);
    buf.extend_from_slice(&filetime.to_be_bytes());
    buf.push(0);
    for part in ["POST", path, "", payload] {
        buf.extend_from_slice(part.as_bytes());
        buf.push(0);
    }
    let sig = sign_p1363(&keys.pem, &buf);
    let mut header = Vec::new();
    header.extend_from_slice(&1u32.to_be_bytes());
    header.extend_from_slice(&filetime.to_be_bytes());
    header.extend_from_slice(&sig);
    b64std(&header)
}
