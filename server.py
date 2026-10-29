#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
Xbox 网页串流 - 本地服务器
职责:
  1. 托管 www/ 下的网页
  2. XAL(SISU) 微软账号登录: 浏览器授权 -> 粘贴回调 URL 换取 v1 令牌
     -> sisu authorize -> XSTS -> xHome gsToken / xccs 远程管理
  3. 将 /v5 /v6 请求代理到 xHome 网关 (带上 gsToken)
  4. /api/msal 提供发给主机的 lpt 用户令牌
     (PURPOSE_XBOX_CLOUD_CONSOLE_TRANSFER_TOKEN; 缺失会导致主机 5 分钟后踢掉会话)
  5. /api/power 通过 xccs.xboxlive.com 远程开关机

协议复刻自开源项目 xal-node / xbox-xcloud-player / Geocld.XStreaming (MIT)。
仅监听 127.0.0.1, 令牌保存在本目录 tokens.json (权限 600)。
"""

import base64
import hashlib
import http.client
import http.server
import json
import mimetypes
import os
import queue
import socket
import ssl
import struct
import subprocess
import sys
import tempfile
import threading
import time
import urllib.parse
import uuid

BASE_DIR = os.path.dirname(os.path.abspath(__file__))
WWW_DIR = os.path.join(BASE_DIR, 'www')
TOKEN_FILE = os.path.join(BASE_DIR, 'tokens.json')
PORT = 8977

# XAL 应用标识 (xal-node 同款)
XAL_APP = {
    'AppId': '000000004c20a908',
    'TitleId': '328178078',
    'RedirectUri': 'ms-xal-000000004c20a908://auth',
}
# 登录跳转使用应用注册的 ms-xal:// 私有协议, 由 XboxLoginHelper.app 自动接收授权码
XAL_UUID_NS = uuid.UUID('6ba7b811-9dad-11d1-80b4-00c04fd430c8')

USER_AGENT = ('Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) '
              'AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36')

_print_lock = threading.Lock()
_token_lock = threading.Lock()

# 令牌缓存 (内存镜像, 落盘到 tokens.json)
_state = {
    'xalkeys': None,       # {pem, x, y} 本地 P-256 证明密钥
    'xaluser': None,       # {access_token, refresh_token, expires_at} login.live.com v1
    'xaldevice': None,     # {token, expires_at} device.auth.xboxlive.com
    'sisu': None,          # sisu /authorize 响应 (UserToken/DeviceToken/TitleToken)
    'xhome': None,         # {gsToken, host, expires_at}
    'webxsts': None,       # {token, uhs, expires_at} XSTS(http://xboxlive.com)
}

_xal_flow = None          # 进行中的 SISU 登录 {verifier, state, session_id, device_token}


def log(fmt, *args):
    with _print_lock:
        print(time.strftime('[%H:%M:%S] ') + (fmt % args if args else fmt), flush=True)


# ---------------------------------------------------------------- HTTP 基础

def http_json(method, host, path, headers, body=None, form=None, want_headers=False):
    """返回 (status, parsed) 或 (status, parsed, headers)。"""
    if form is not None:
        body = urllib.parse.urlencode(form).encode('utf-8')
        headers = dict(headers or {})
        headers.setdefault('Content-Type', 'application/x-www-form-urlencoded')
    conn = http.client.HTTPSConnection(host, timeout=25, context=ssl.create_default_context())
    try:
        conn.request(method, path, body=body, headers=headers or {})
        resp = conn.getresponse()
        raw = resp.read()
        try:
            data = json.loads(raw.decode('utf-8', 'replace'))
        except Exception:
            data = raw.decode('utf-8', 'replace')
        if want_headers:
            return resp.status, data, dict(resp.getheaders())
        return resp.status, data
    finally:
        conn.close()


def save_tokens():
    """原子写 (临时文件+rename), 避免多线程并发保存写坏 tokens.json。"""
    tmp = {k: v for k, v in _state.items() if v}
    fd, tmpfile = tempfile.mkstemp(dir=BASE_DIR, prefix='.tokens-', suffix='.tmp')
    try:
        with os.fdopen(fd, 'w') as f:
            json.dump(tmp, f, ensure_ascii=False, indent=2)
        os.chmod(tmpfile, 0o600)
        os.replace(tmpfile, TOKEN_FILE)
    except Exception:
        try:
            os.unlink(tmpfile)
        except OSError:
            pass
        raise


def load_tokens():
    if not os.path.exists(TOKEN_FILE):
        return
    try:
        with open(TOKEN_FILE) as f:
            data = json.load(f)
        _state.update({k: v for k, v in data.items() if k in _state})
        log('已加载本地令牌 (上次登录保留)')
    except Exception as e:
        log('读取 tokens.json 失败: %s', e)


# ---------------------------------------------------------------- XAL 密钥与签名

def _b64url(b):
    return base64.urlsafe_b64encode(b).rstrip(b'=').decode()


def xal_ensure_keys():
    keys = _state.get('xalkeys')
    if keys and keys.get('pem'):
        return keys
    pem = subprocess.run(['openssl', 'ecparam', '-name', 'prime256v1', '-genkey', '-noout'],
                         capture_output=True, check=True).stdout.decode()
    der = subprocess.run(['openssl', 'ec', '-in', '/dev/stdin', '-pubout', '-outform', 'DER'],
                         input=pem.encode(), capture_output=True, check=True).stdout
    if len(der) < 65 or der[-65] != 0x04:
        raise RuntimeError('解析 P-256 公钥失败')
    point = der[-64:]
    keys = {'pem': pem, 'x': _b64url(point[:32]), 'y': _b64url(point[32:])}
    _state['xalkeys'] = keys
    save_tokens()
    return keys


def _der_to_p1363(der):
    """DER ECDSA 签名 -> 64 字节 r||s。"""
    if der[0] != 0x30:
        raise RuntimeError('签名 DER 解析失败')
    i = 2
    if der[1] & 0x80:
        i = 2 + (der[1] & 0x7f)
    def read_int(pos):
        if der[pos] != 0x02:
            raise RuntimeError('签名 DER 解析失败')
        n = der[pos + 1]
        return der[pos + 2:pos + 2 + n]
    r = read_int(i)
    s = read_int(i + 2 + len(r))
    r = r.lstrip(b'\x00').rjust(32, b'\x00')
    s = s.lstrip(b'\x00').rjust(32, b'\x00')
    return r + s


def xal_sign(path, authorization_token, payload):
    """Xbox ProofOfPossession 签名 (xal-node sign() 的移植)。"""
    keys = xal_ensure_keys()
    filetime = (int(time.time()) + 11644473600) * 10000000
    buf = struct.pack('>I', 1) + b'\x00' + struct.pack('>Q', filetime) + b'\x00'
    for part in ('POST', path, authorization_token, payload):
        buf += part.encode() + b'\x00'
    fd, keyfile = tempfile.mkstemp()
    try:
        os.write(fd, keys['pem'].encode())
        os.close(fd)
        os.chmod(keyfile, 0o600)
        der = subprocess.run(['openssl', 'dgst', '-sha256', '-sign', keyfile],
                             input=buf, capture_output=True, check=True).stdout
    finally:
        try:
            os.unlink(keyfile)
        except OSError:
            pass
    sig = _der_to_p1363(der)
    header = struct.pack('>I', 1) + struct.pack('>Q', filetime) + sig
    return base64.b64encode(header).decode()


def xal_proof_key():
    keys = xal_ensure_keys()
    return {'use': 'sig', 'alg': 'ES256', 'kty': 'EC', 'crv': 'P-256',
            'x': keys['x'], 'y': keys['y']}


def _next_uuid():
    return str(uuid.uuid3(XAL_UUID_NS, str(int(time.time() * 1000))))


# ---------------------------------------------------------------- XAL 流程

def xal_device_token(force=False):
    cur = _state.get('xaldevice')
    if not force and cur and cur.get('expires_at', 0) - 300 > time.time():
        return cur['token']
    payload = {
        'Properties': {
            'AuthMethod': 'ProofOfPossession',
            'Id': '{%s}' % _next_uuid(),
            'DeviceType': 'Android',
            'SerialNumber': '{%s}' % _next_uuid(),
            'Version': '15.0',
            'ProofKey': xal_proof_key(),
        },
        'RelyingParty': 'http://auth.xboxlive.com',
        'TokenType': 'JWT',
    }
    body = json.dumps(payload)
    status, data = http_json('POST', 'device.auth.xboxlive.com', '/device/authenticate', {
        'Content-Type': 'application/json',
        'x-xbl-contract-version': '1',
        'Cache-Control': 'no-store, must-revalidate, no-cache',
        'Signature': xal_sign('/device/authenticate', '', body),
    }, body=body.encode())
    if status != 200 or not isinstance(data, dict) or 'Token' not in data:
        raise RuntimeError('设备令牌获取失败: %s' % json.dumps(data)[:300])
    _state['xaldevice'] = {'token': data['Token'],
                           'expires_at': time.time() + 36000}
    save_tokens()
    return data['Token']


def xal_begin_login():
    """生成 SISU 登录 URL。"""
    global _xal_flow
    dt = xal_device_token()
    verifier = _b64url(os.urandom(32))
    challenge = _b64url(hashlib.sha256(verifier.encode()).digest())
    state = _b64url(os.urandom(48))
    payload = {
        'AppId': XAL_APP['AppId'],
        'TitleId': XAL_APP['TitleId'],
        'RedirectUri': XAL_APP['RedirectUri'],
        'DeviceToken': dt,
        'Sandbox': 'RETAIL',
        'TokenType': 'code',
        'Offers': ['service::user.auth.xboxlive.com::MBI_SSL'],
        'Query': {
            'display': 'android_phone',
            'code_challenge': challenge,
            'code_challenge_method': 'S256',
            'state': state,
        },
    }
    body = json.dumps(payload)
    status, data, headers = http_json('POST', 'sisu.xboxlive.com', '/authenticate', {
        'Content-Type': 'application/json',
        'x-xbl-contract-version': '1',
        'Cache-Control': 'no-store, must-revalidate, no-cache',
        'Signature': xal_sign('/authenticate', '', body),
    }, body=body.encode(), want_headers=True)
    if status != 200 or not isinstance(data, dict) or 'MsaOauthRedirect' not in data:
        raise RuntimeError('SISU 会话创建失败: %s' % json.dumps(data)[:300])
    session_id = headers.get('x-sessionid') or headers.get('X-SessionId') or ''
    _xal_flow = {
        'verifier': verifier,
        'state': state,
        'session_id': session_id,
        'device_token': dt,
    }
    params = data.get('MsaRequestParameters') or {}
    # 保持原始 ms-xal:// 跳转: 由本目录的 XboxLoginHelper.app (注册了该协议) 自动接收授权码;
    # 手动粘贴回调地址仍然兼容
    sep = '&' if '?' in data['MsaOauthRedirect'] else '?'
    return data['MsaOauthRedirect'] + sep + urllib.parse.urlencode(params)


def xal_user_access():
    """v1 (login.live.com) 用户令牌, 自动刷新 (加锁防并发刷新轮换竞态)。"""
    with _token_lock:
        u = _state.get('xaluser')
        if u and u.get('expires_at', 0) - 60 > time.time() and u.get('access_token'):
            return u['access_token']
        if not u or not u.get('refresh_token'):
            raise RuntimeError('未登录')
        status, data = http_json('POST', 'login.live.com', '/oauth20_token.srf', {}, form={
            'client_id': XAL_APP['AppId'],
            'grant_type': 'refresh_token',
            'refresh_token': u['refresh_token'],
            'scope': 'service::user.auth.xboxlive.com::MBI_SSL',
        })
        if status != 200 or 'access_token' not in data:
            _state['xaluser'] = None
            save_tokens()
            raise RuntimeError('刷新登录令牌失败, 请重新登录: %s' % json.dumps(data)[:200])
        _state['xaluser'] = {
            'access_token': data['access_token'],
            'refresh_token': data.get('refresh_token', u['refresh_token']),
            'expires_at': time.time() + int(data.get('expires_in', 3600)),
        }
        save_tokens()
        return _state['xaluser']['access_token']


def xal_complete_login(url_or_code):
    """用浏览器回调 URL (ms-xal://...?code=...) 或纯 code 完成登录。"""
    global _xal_flow
    if not _xal_flow:
        raise RuntimeError('没有进行中的登录, 请先重新发起')
    code = url_or_code.strip()
    if 'code=' in code:
        q = urllib.parse.urlparse(code).query or code.split('?', 1)[-1]
        params = urllib.parse.parse_qs(q)
        if 'code' not in params:
            raise RuntimeError('粘贴的内容里没有找到 code= 参数')
        code = params['code'][0]
    flow, _xal_flow = _xal_flow, None
    status, data = http_json('POST', 'login.live.com', '/oauth20_token.srf', {}, form={
        'client_id': XAL_APP['AppId'],
        'code': code,
        'code_verifier': flow['verifier'],
        'grant_type': 'authorization_code',
        'redirect_uri': XAL_APP['RedirectUri'],
        'scope': 'service::user.auth.xboxlive.com::MBI_SSL',
    })
    if status != 200 or 'access_token' not in data:
        raise RuntimeError('授权码换取失败(可能已过期, 请重新发起): %s' % json.dumps(data)[:200])
    _state['xaluser'] = {
        'access_token': data['access_token'],
        'refresh_token': data.get('refresh_token'),
        'expires_at': time.time() + int(data.get('expires_in', 3600)),
    }

    payload = {
        'AccessToken': 't=' + data['access_token'],
        'AppId': XAL_APP['AppId'],
        'DeviceToken': flow['device_token'],
        'Sandbox': 'RETAIL',
        'SiteName': 'user.auth.xboxlive.com',
        'UseModernGamertag': True,
        'ProofKey': xal_proof_key(),
        'SessionId': flow['session_id'],
    }
    body = json.dumps(payload)
    status, sdata = http_json('POST', 'sisu.xboxlive.com', '/authorize', {
        'Content-Type': 'application/json',
        'x-xbl-contract-version': '1',
        'Cache-Control': 'no-store, must-revalidate, no-cache',
        'Signature': xal_sign('/authorize', '', body),
    }, body=body.encode())
    if status != 200 or not isinstance(sdata, dict) or 'UserToken' not in sdata:
        _state['xaluser'] = None
        save_tokens()
        raise RuntimeError('SISU 授权失败: %s' % json.dumps(sdata)[:300])
    _state['sisu'] = sdata
    _state['xhome'] = None
    _state['webxsts'] = None
    save_tokens()
    log('XAL 登录完成')
    try:
        get_xhome_token()
    except Exception as e:
        log('预取 xHome 令牌失败(稍后再试): %s', e)


def xal_xsts(relying_party, force=False):
    """基于 sisu 令牌的 XSTS (带 ProofOfPossession 签名)。
    注: 只传 UserTokens 即可 (XStreaming 同款); 我们的 sisu 响应里没有顶层 DeviceToken。"""
    sisu = _state.get('sisu')
    if not sisu:
        raise RuntimeError('未完成 XAL 登录')
    payload = {
        'Properties': {
            'SandboxId': 'RETAIL',
            'UserTokens': [sisu['UserToken']['Token']],
        },
        'RelyingParty': relying_party,
        'TokenType': 'JWT',
    }
    body = json.dumps(payload)
    status, data = http_json('POST', 'xsts.auth.xboxlive.com', '/xsts/authorize', {
        'Content-Type': 'application/json',
        'x-xbl-contract-version': '1',
        'Cache-Control': 'no-store, must-revalidate, no-cache',
        'Signature': xal_sign('/xsts/authorize', '', body),
    }, body=body.encode())
    if status != 200 or 'Token' not in data:
        raise RuntimeError('XSTS(%s) 失败: %s' % (relying_party, json.dumps(data)[:300]))
    return data


def get_xhome_token(force=False):
    """(gsToken, host)。缓存, 过期前 60s 自动刷新。"""
    with _token_lock:
        cur = _state.get('xhome')
        if not force and cur and cur.get('expires_at', 0) - 60 > time.time():
            return cur['gsToken'], cur['host']
        xsts = xal_xsts('http://gssv.xboxlive.com/', force=force)
        status, data = http_json('POST', 'xhome.gssv-play-prod.xboxlive.com',
                                 '/v2/login/user', {
                                     'Content-Type': 'application/json',
                                     'x-gssv-client': 'XboxComBrowser',
                                 }, body=json.dumps({
                                     'token': xsts['Token'],
                                     'offeringId': 'xhome',
                                 }).encode())
        if status != 200 or 'gsToken' not in data:
            raise RuntimeError('获取 xHome 串流令牌失败: %s' % json.dumps(data)[:300])
        region = None
        for r in data.get('offeringSettings', {}).get('regions', []):
            if r.get('isDefault'):
                region = r
                break
        if region is None:
            regions = data.get('offeringSettings', {}).get('regions', [])
            if not regions:
                raise RuntimeError('xHome 令牌响应中没有可用区域')
            region = regions[0]
        _state['xhome'] = {
            'gsToken': data['gsToken'],
            'host': region['baseUri'].replace('https://', '').rstrip('/'),
            'expires_at': time.time() + int(data.get('durationInSeconds', 300)),
        }
        save_tokens()
        log('xHome 令牌已就绪, 网关: %s', _state['xhome']['host'])
        return _state['xhome']['gsToken'], _state['xhome']['host']


def get_web_xsts(force=False):
    """XSTS (http://xboxlive.com), 用于 xccs 远程管理。"""
    with _token_lock:
        cur = _state.get('webxsts')
        if not force and cur and cur.get('expires_at', 0) - 60 > time.time():
            return cur['uhs'], cur['token']
        data = xal_xsts('http://xboxlive.com', force=force)
        _state['webxsts'] = {
            'token': data['Token'],
            'uhs': data['DisplayClaims']['xui'][0]['uhs'],
            'expires_at': time.time() + 3000,
        }
        save_tokens()
        return _state['webxsts']['uhs'], _state['webxsts']['token']


_lpt_lock = threading.Lock()
_lpt_cache = {'token': None, 'ts': 0}


def get_lpt():
    """发给主机的 transfer token。必须有, 否则主机 5 分钟后断开会话。
    90 秒缓存: 页面就绪通知与兜底重试可能在短时间内重复请求。"""
    with _lpt_lock:
        if _lpt_cache['token'] and time.time() - _lpt_cache['ts'] < 90:
            return _lpt_cache['token']
        xal_user_access()
        status, data = http_json('POST', 'login.live.com', '/oauth20_token.srf', {}, form={
            'client_id': XAL_APP['AppId'],
            'scope': 'service::http://Passport.NET/purpose::PURPOSE_XBOX_CLOUD_CONSOLE_TRANSFER_TOKEN',
            'grant_type': 'refresh_token',
            'refresh_token': _state['xaluser']['refresh_token'],
        })
        # 该接口的响应字段名是 lpt (不是标准的 access_token)
        token = None
        if isinstance(data, dict):
            token = data.get('lpt') or data.get('access_token')
        if status != 200 or not token:
            raise RuntimeError('获取主机握手令牌(lpt)失败: %s' % json.dumps(data)[:300])
        _lpt_cache['token'] = token
        _lpt_cache['ts'] = time.time()
        return token


# ---------------------------------------------------------------- 直播源 (HTTP-FLV)

_live_lock = threading.Lock()
_live = {
    'ffmpeg': None,        # subprocess
    'sid': None,           # 推流会话代次 (防 stop/start 竞态误杀)
    'clients': [],         # [queue.Queue] 每个拉流客户端一个队列
    'frame_buf': b'',      # 自最近一个视频关键帧起的数据 (新客户端从这里接入)
    'stats': {'in_bytes': 0, 'started': 0},
}


def _find_ffmpeg():
    cand = os.path.join(BASE_DIR, 'bin', 'ffmpeg')
    if os.path.isfile(cand) and os.access(cand, os.X_OK):
        return cand
    return 'ffmpeg'


def _is_video_keyframe(chunk, start):
    """在 flv 数据里判断 start 处是否为视频关键帧 tag (tagtype 9, frametype 1)。"""
    if start + 12 > len(chunk):
        return False
    if chunk[start] != 0x09:
        return False
    return bool(chunk[start + 12] & 0xF0) == 0x10


def _live_reader(stdout):
    """读 ffmpeg 输出的 FLV 流: tag 级解析。新客户端接入点 = FLV头+配置tag(元数据/
    AVC/AAC sequence header) + 最近一个视频关键帧起的 tags; 全量数据实时广播。"""
    buf = bytearray()          # 待解析 (可能 tag 不完整)
    kf = bytearray()           # 接入缓冲: config + 最近关键帧起的 tags
    config = bytearray()       # FLV头 + 一次性配置 tag
    config_last_total = 0      # config 段最后一个 tag 的总长 (11+datasize), 用于修正拼接处的 prev-size
    header_seen = False
    try:
        while True:
            data = stdout.read(16384)
            if not data:
                break
            buf.extend(data)
            pos = 0
            if not header_seen:
                if len(buf) < 9:
                    continue
                config = bytearray(buf[:9])
                header_seen = True
                pos = 9
            while len(buf) - pos >= 15:
                tagtype = buf[pos + 4]
                datasize = int.from_bytes(buf[pos + 5:pos + 8], 'big')
                total = 4 + 11 + datasize
                if len(buf) - pos < total:
                    break
                tag = buf[pos:pos + total]
                if tagtype == 18:
                    # onMetaData 等脚本 tag: 归入配置段
                    config.extend(tag)
                    config_last_total = total
                elif tagtype == 9:
                    b0 = buf[pos + 15]
                    b1 = buf[pos + 16] if total > 16 else 0
                    if (b0 & 0xF0) == 0x10 and b1 == 0x00:
                        config.extend(tag)   # AVC sequence header
                        config_last_total = total
                    elif (b0 & 0xF0) == 0x10 and b1 == 0x01:
                        # 视频关键帧 NAL: 重置接入缓冲; 拼接处 prev-size 指向 config 末 tag
                        kf = bytearray(config)
                        fixed = bytearray(tag)
                        fixed[0:4] = config_last_total.to_bytes(4, 'big')
                        kf.extend(fixed)
                    else:
                        kf.extend(tag)
                        if len(kf) > 12 * 1024 * 1024:
                            kf = bytearray(config)  # 关键帧间隔异常, 等下一个
                elif tagtype == 8:
                    if buf[pos + 15] == 0x00:
                        config.extend(tag)   # AAC sequence header
                        config_last_total = total
                    else:
                        kf.extend(tag)
                pos += total
            del buf[:pos]
            with _live_lock:
                _live['frame_buf'] = bytes(kf)
                _live['stats']['in_bytes'] += len(data)
                for q in list(_live['clients']):
                    try:
                        q.put_nowait(data)
                    except queue.Full:
                        # 拉流端(如播放器预填大缓存)暂时不读: 清空队列并请求重同步,
                        # 客户端会从最近关键帧重新接入, 绝不踢掉连接
                        try:
                            while True:
                                q.get_nowait()
                        except queue.Empty:
                            pass
                        try:
                            q.put_nowait(b'RESYNC')
                        except Exception:
                            pass
                    except Exception:
                        _live['clients'].remove(q)
    except Exception:
        pass
    with _live_lock:
        for q in list(_live['clients']):
            try:
                q.put_nowait(b'')  # 结束信号
            except Exception:
                pass


def get_lan_ip():
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        s.connect(('8.8.8.8', 80))
        return s.getsockname()[0]
    except Exception:
        return '127.0.0.1'
    finally:
        s.close()


def live_start():
    with _live_lock:
        if _live['ffmpeg'] and _live['ffmpeg'].poll() is None:
            return {'ok': True, 'already': True, 'sid': _live['sid']}
        ff = _find_ffmpeg()
        hls_dir = '/tmp/xwr-hls'
        try:
            os.makedirs(hls_dir, exist_ok=True)
        except Exception:
            pass
        hls_path = os.path.join(hls_dir, 'live.m3u8')
        try:
            fflog = open('/tmp/xwr-ffmpeg.log', 'wb')
            # 注意: ffmpeg 的输出选项只作用于紧随其后的输出, 两个输出必须各自指定编码
            # FLV 输出用 VideoToolbox 硬编限制平均码率 8Mbps (适配直播平台); HLS 局域网观看保持直通高码率
            proc = subprocess.Popen(
                [ff, '-hide_banner', '-loglevel', 'info',
                 '-hwaccel', 'videotoolbox',
                 '-fflags', '+genpts', '-f', 'matroska', '-i', 'pipe:0',
                 '-map', '0:v:0', '-map', '0:a:0',
                 '-c:v', 'h264_videotoolbox', '-allow_sw', '1', '-realtime', '1',
                 '-b:v', '8M', '-maxrate', '8.5M', '-bufsize', '16M',
                 '-c:a', 'aac', '-b:a', '128k',
                 '-f', 'flv', 'pipe:1',
                 '-map', '0:v:0', '-map', '0:a:0',
                 '-c:v', 'copy', '-c:a', 'aac', '-b:a', '128k',
                 '-f', 'hls', '-hls_time', '2', '-hls_list_size', '8',
                 '-hls_flags', 'delete_segments+independent_segments',
                 '-hls_segment_filename', os.path.join(hls_dir, 'seg_%04d.ts'),
                 hls_path],
                stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                stderr=fflog)
        except Exception as e:
            return {'ok': False, 'error': '启动 ffmpeg 失败: %s' % e}
        _live['ffmpeg'] = proc
        _live['sid'] = str(uuid.uuid4())
        _live['frame_buf'] = b''
        _live['stats'] = {'in_bytes': 0, 'started': time.time()}
        threading.Thread(target=_live_reader, args=(proc.stdout,), daemon=True).start()
    log('直播源已启动 (HTTP-FLV: /live.flv)')
    return {'ok': True, 'sid': _live['sid']}


def live_stop():
    with _live_lock:
        proc = _live['ffmpeg']
        _live['ffmpeg'] = None
        _live['sid'] = None
        _live['frame_buf'] = b''
    if proc and proc.poll() is None:
        try:
            proc.stdin.close()
            proc.wait(timeout=3)
        except Exception:
            try:
                proc.kill()
            except Exception:
                pass
    try:
        for f in os.listdir('/tmp/xwr-hls'):
            os.unlink(os.path.join('/tmp/xwr-hls', f))
    except Exception:
        pass
    log('直播源已停止')
    return {'ok': True}


def live_status():
    with _live_lock:
        running = bool(_live['ffmpeg'] and _live['ffmpeg'].poll() is None)
        return {'running': running,
                'clients': len(_live['clients']),
                'in_bytes': _live['stats']['in_bytes'],
                'lan_ip': get_lan_ip()}


def live_stream(handler):
    """GET /live.flv: 先等一个有效关键帧接入点, 再加入广播列表收实时数据
    (顺序不能反, 否则等待期间积压的实时数据会与关键帧头交错损坏流)。"""
    with _live_lock:
        running = bool(_live['ffmpeg'] and _live['ffmpeg'].poll() is None)
    if not running:
        handler._json({'error': '直播源未启动, 请先在页面开启推流'}, 503)
        return
    # 阶段一: 等待关键帧接入点 (最多 20 秒), 此时尚未订阅广播
    head = None
    waited = 0
    while waited < 20:
        with _live_lock:
            running = bool(_live['ffmpeg'] and _live['ffmpeg'].poll() is None)
            head = _live['frame_buf']
        if not running:
            handler._json({'error': '直播源已停止'}, 503)
            return
        if head:
            break
        time.sleep(0.5)
        waited += 0.5
    if not head:
        handler._json({'error': '等待关键帧超时, 请重新开始推流'}, 504)
        return
    handler.send_response(200)
    handler.send_header('Content-Type', 'video/x-flv')
    handler.send_header('Cache-Control', 'no-store')
    handler.send_header('Connection', 'close')
    handler.send_header('Transfer-Encoding', 'chunked')
    handler.end_headers()
    # 阶段二: 订阅广播并循环发送
    q = queue.Queue(maxsize=2000)
    with _live_lock:
        _live['clients'].append(q)
    try:
        def send(b):
            if not b:
                return
            handler.wfile.write(('%x\r\n' % len(b)).encode() + b + b'\r\n')
        send(head)
        while True:
            item = q.get()
            if not item:
                break
            if item == b'RESYNC':
                # 从最近关键帧重新接入 (队列被清空过)
                try:
                    with _live_lock:
                        live_head = _live['frame_buf']
                    send(live_head)
                except Exception:
                    break
                continue
            send(item)
    except Exception:
        pass
    finally:
        with _live_lock:
            if q in _live['clients']:
                _live['clients'].remove(q)
        handler.close_connection = True


# ---------------------------------------------------------------- HTTP 服务

class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.1'
    server_version = 'XboxWebRemote/1.1'

    def log_message(self, fmt, *args):
        pass

    def _json(self, obj, status=200):
        body = json.dumps(obj, ensure_ascii=False).encode('utf-8')
        self.send_response(status)
        self.send_header('Content-Type', 'application/json; charset=utf-8')
        self.send_header('Content-Length', str(len(body)))
        self.send_header('Cache-Control', 'no-store')
        self.end_headers()
        self.wfile.write(body)

    def _read_body(self):
        length = int(self.headers.get('Content-Length') or 0)
        return self.rfile.read(length) if length > 0 else b''

    def _static(self, path):
        if path == '/':
            path = '/index.html'
        fname = os.path.normpath(os.path.join(WWW_DIR, path.lstrip('/')))
        if not fname.startswith(WWW_DIR) or not os.path.isfile(fname):
            self._json({'error': 'not found'}, 404)
            return
        ctype = mimetypes.guess_type(fname)[0] or 'application/octet-stream'
        with open(fname, 'rb') as f:
            body = f.read()
        self.send_response(200)
        self.send_header('Content-Type', ctype)
        self.send_header('Content-Length', str(len(body)))
        self.send_header('Cache-Control', 'no-cache')
        self.end_headers()
        self.wfile.write(body)

    def _route(self, method):
        path = urllib.parse.urlparse(self.path).path
        if path == '/live.flv':
            return live_stream(self)
        if path.startswith('/api/'):
            return self._api(method, path)
        if path.startswith('/v5') or path.startswith('/v6') or path.startswith('/v2'):
            return self._proxy(method, path)
        if method == 'GET':
            return self._static(path)
        self._json({'error': 'method not allowed'}, 405)

    def _api(self, method, path):
        global _xal_flow
        try:
            if path == '/api/auth/status' and method == 'GET':
                logged = bool(_state.get('xaluser') and _state.get('sisu'))
                detail = ''
                if logged:
                    try:
                        get_xhome_token()
                    except Exception as e:
                        detail = str(e)
                        logged = False
                return self._json({'authenticated': logged, 'detail': detail})

            if path == '/api/auth/start' and method == 'POST':
                url = xal_begin_login()
                return self._json({'url': url})

            if path == '/api/auth/code' and method == 'POST':
                body = json.loads(self._read_body() or b'{}')
                code = (body or {}).get('code') or ''
                if not code:
                    return self._json({'error': '请粘贴登录后跳转的完整地址'}, 400)
                xal_complete_login(code)
                return self._json({'status': 'ok'})

            if path == '/api/auth/logout' and method == 'POST':
                for k in list(_state):
                    _state[k] = None
                _xal_flow = None
                if os.path.exists(TOKEN_FILE):
                    os.remove(TOKEN_FILE)
                log('已退出登录, 令牌已删除')
                return self._json({'status': 'ok'})

            if path == '/api/msal' and method == 'GET':
                try:
                    return self._json({'token': get_lpt()})
                except Exception as e:
                    return self._json({'error': str(e)}, 401)

            if path == '/api/power' and method == 'POST':
                body = json.loads(self._read_body() or b'{}')
                server_id = body.get('serverId')
                action = body.get('action')
                if not server_id or action not in ('on', 'off'):
                    return self._json({'error': '参数错误, 需要 serverId 和 action(on/off)'}, 400)
                try:
                    uhs, token = get_web_xsts()
                    command = 'WakeUp' if action == 'on' else 'TurnOff'
                    payload = {
                        'destination': 'Xbox',
                        'type': 'Power',
                        'command': command,
                        'sessionId': str(uuid.uuid4()),
                        'sourceId': 'com.microsoft.smartglass',
                        'parameters': [],
                        'linkedXboxId': server_id,
                    }
                    status, data = http_json('POST', 'xccs.xboxlive.com', '/commands', {
                        'Authorization': 'XBL3.0 x=%s;%s' % (uhs, token),
                        'Accept-Language': 'en-US',
                        'skillplatform': 'RemoteManagement',
                        'x-xbl-contract-version': '4',
                        'x-xbl-client-name': 'XboxApp',
                        'x-xbl-client-type': 'UWA',
                        'x-xbl-client-version': '39.39.22001.0',
                        'Content-Type': 'application/json',
                    }, body=json.dumps(payload).encode())
                except Exception as e:
                    log('电源命令鉴权失败: %s', e)
                    return self._json({'error': str(e)}, 401)
                log('电源命令 %s %s -> HTTP %s', command, server_id, status)
                if 200 <= status < 300:
                    return self._json({'ok': True, 'command': command, 'result': data})
                return self._json({'ok': False, 'status': status, 'detail': data}, 502)

            if path == '/api/live/start' and method == 'POST':
                r = live_start()
                return self._json(r, 200 if r.get('ok') else 500)

            if path == '/api/live/stop' and method == 'POST':
                return self._json(live_stop())

            if path == '/api/live/status' and method == 'GET':
                return self._json(live_status())

            if path == '/api/live/chunk' and method == 'POST':
                body = self._read_body()
                with _live_lock:
                    proc = _live['ffmpeg']
                    sid = _live.get('sid')
                if not (proc and proc.poll() is None):
                    return self._json({'error': '直播源未启动'}, 409)
                if sid and self.headers.get('X-Live-Sid') not in (None, '', sid):
                    return self._json({'error': '推流会话已更替'}, 409)
                try:
                    proc.stdin.write(body)
                    proc.stdin.flush()
                except Exception as e:
                    return self._json({'error': '推流管道已断: %s' % e}, 502)
                return self._json({'ok': True})

            return self._json({'error': 'unknown api'}, 404)
        except Exception as e:
            log('API 错误 %s: %s', path, e)
            return self._json({'error': str(e)}, 500)

    def _proxy(self, method, path):
        body = self._read_body()
        try:
            token, host = get_xhome_token()
        except Exception as e:
            return self._json({'error': '尚未登录或令牌失效', 'detail': str(e)}, 401)
        headers = {
            'Accept': self.headers.get('Accept', 'application/json'),
            'Content-Type': self.headers.get('Content-Type', 'application/json'),
            'Authorization': 'Bearer ' + token,
            'X-Gssv-Client': 'XboxComBrowser',
            'User-Agent': USER_AGENT,
        }
        for h in ('X-Ms-Device-Info', 'X-MS-Device-Info'):
            if self.headers.get(h):
                headers['X-MS-Device-Info'] = self.headers[h]
                break
        if body:
            headers['Content-Length'] = str(len(body))
        if '/play' in path and body:
            try:
                bj = json.loads(body)
                log('PLAY 请求 osName=%s devOs=%s',
                    bj.get('settings', {}).get('osName'),
                    json.loads(self.headers.get('X-MS-Device-Info', '{}')).get('dev', {}).get('os', {}).get('name'))
            except Exception:
                pass
        t0 = time.time()
        try:
            conn = http.client.HTTPSConnection(host, timeout=35,
                                               context=ssl.create_default_context())
            try:
                conn.request(method, path, body=body if body else None, headers=headers)
                resp = conn.getresponse()
                raw = resp.read()
                status = resp.status
                ctype = resp.getheader('Content-Type', 'application/json')
            finally:
                conn.close()
        except Exception as e:
            log('代理失败 %s %s -> %s', method, path, e)
            return self._json({'error': '网关请求失败', 'detail': str(e)}, 502)
        log('%s %s%s -> %d (%dms, %dB)',
            method, host, path, status, int((time.time() - t0) * 1000), len(raw))
        self.send_response(status)
        if raw:
            self.send_header('Content-Type', ctype)
        self.send_header('Content-Length', str(len(raw)))
        self.send_header('Cache-Control', 'no-store')
        self.end_headers()
        if raw:
            self.wfile.write(raw)

    def do_GET(self):
        self._route('GET')

    def do_POST(self):
        self._route('POST')

    def do_DELETE(self):
        self._route('DELETE')


# ---------------------------------------------------------------- 局域网 HLS 服务 (8978, 只读分片)

class HlsHandler(http.server.BaseHTTPRequestHandler):
    """只服务 /hls/ 下的 m3u8/ts, 供局域网设备观看; 不暴露任何 API 与令牌。"""
    protocol_version = 'HTTP/1.1'
    server_version = 'XboxWebRemote-HLS/1.0'

    def log_message(self, fmt, *args):
        pass

    def do_GET(self):
        path = urllib.parse.urlparse(self.path).path
        if not path.startswith('/hls/'):
            self.send_response(404)
            self.send_header('Content-Length', '0')
            self.end_headers()
            return
        name = os.path.normpath(path[len('/hls/'):])
        if name.startswith('..') or os.path.isabs(name) or not (name.endswith('.m3u8') or name.endswith('.ts')):
            self.send_response(403)
            self.send_header('Content-Length', '0')
            self.end_headers()
            return
        full = os.path.join('/tmp/xwr-hls', name)
        if not os.path.isfile(full):
            self.send_response(404)
            self.send_header('Content-Length', '0')
            self.end_headers()
            return
        with open(full, 'rb') as f:
            body = f.read()
        ctype = 'application/vnd.apple.mpegurl' if name.endswith('.m3u8') else 'video/mp2t'
        self.send_response(200)
        self.send_header('Content-Type', ctype)
        self.send_header('Content-Length', str(len(body)))
        self.send_header('Cache-Control', 'no-store')
        self.end_headers()
        self.wfile.write(body)


def start_hls_server():
    try:
        httpd = http.server.ThreadingHTTPServer(('0.0.0.0', 8978), HlsHandler)
        threading.Thread(target=httpd.serve_forever, daemon=True).start()
        log('局域网 HLS 观看服务: http://%s:8978/hls/live.m3u8  (仅推送时可用)', get_lan_ip())
    except Exception as e:
        log('HLS 服务启动失败(8978 被占用?): %s', e)


def main():
    load_tokens()
    if not os.path.isdir(WWW_DIR):
        log('缺少 www/ 目录, 请保持文件夹完整')
        sys.exit(1)
    start_hls_server()
    httpd = http.server.ThreadingHTTPServer(('127.0.0.1', PORT), Handler)
    log('Xbox 网页串流服务已启动: http://127.0.0.1:%d  (Ctrl+C 退出)', PORT)
    try:
        httpd.serve_forever()
    except KeyboardInterrupt:
        log('再见!')


if __name__ == '__main__':
    main()
