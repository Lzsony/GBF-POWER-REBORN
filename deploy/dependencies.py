"""Python-free remote discovery and narrowly scoped APT bootstrap."""
from pathlib import Path
import re
import shutil
import sys

BOOTSTRAP = Path(__file__).with_name('bootstrap.sh')
ERRORS = {
    'ROOT_REQUIRED': '管理帳號無法使用 sudo -n；請先設定管理權限。',
    'BASE_TOOLS_REQUIRED': '缺少 APT、systemd 或基本系統工具；請先修復作業系統環境。',
    'SYSTEMD_REQUIRED': '需要正常運行的 systemd；不會自動更換 init 系統。',
    'HOST_FACTS_UNAVAILABLE': '無法讀取作業系統或主機身份。',
    'UNSUPPORTED_OS': '不支援此 Linux 發行版或版本。',
    'UNSUPPORTED_ARCH': '僅支援 amd64／arm64。',
    'INVALID_MACHINE_ID': '主機缺少有效且穩定的 machine-id。',
    'INVALID_MODE': '不支援此依賴操作。',
    'HOST_IDENTITY_CHANGED': '主機身份在檢查後改變，已停止。',
    'HOST_LOCK_TIMEOUT': '等待主機依賴安裝鎖超過 120 秒；請檢查其他安裝程序或殘留鎖。',
    'APT_LOCK_TIMEOUT': '套件管理鎖無法取得；請等待其他 APT／dpkg 操作結束後重試。',
    'INSTALLED_PACKAGE_BROKEN': '套件已安裝但必要命令或 CA 遺失；請人工修復該套件，再執行 --install-deps。',
    'PACKAGE_DATABASE_BROKEN': 'dpkg 存在未完成或損壞的套件狀態；請先人工修復。',
    'LOG_PATH_CONFLICT': '依賴安裝日誌目錄需為 root 擁有的 0700 實體目錄。',
    'APT_UPDATE_FAILED': '套件索引更新失敗；請檢查網路、套件來源與伺服器端安裝日誌。',
    'APT_SIMULATION_FAILED': '套件安裝模擬失敗；請檢查套件來源與依賴衝突。',
    'EXISTING_PACKAGE_CHANGE': '安裝需要改動既有套件；已拒絕升級、重裝或移除，請人工處理。',
    'APT_INSTALL_FAILED': '套件安裝失敗；保留已完成項目，請檢查伺服器端日誌後重試。',
    'STILL_MISSING': '安裝後仍缺少必要命令或 CA；不繼續部署，請人工檢查。',
}


def check_local(external=False):
    if sys.version_info < (3, 9): raise RuntimeError('部署工具需要本機 Python 3.9+。')
    for tool in ('ssh', 'ssh-keyscan') if external else ('ssh',):
        if not shutil.which(tool): raise RuntimeError('本機缺少 ' + tool + '；請先安裝 OpenSSH 用戶端。')


def parse_report(payload):
    result = {'missingDependencies': [], 'checksComplete': False}
    for line in payload.decode().splitlines():
        fields = line.split('\t')
        if fields[0] == 'host' and len(fields) == 5:
            _, distro, version, arch, identity = fields
            if (distro, version) not in {('debian', '12'), ('debian', '13'), ('ubuntu', '24.04'), ('ubuntu', '26.04')} or arch not in ('amd64', 'arm64') or not re.fullmatch('[0-9a-f]{64}', identity):
                raise ValueError('Invalid dependency host facts')
            result.update(distribution=distro, version=version, architecture=arch, machineIdHash=identity)
        elif fields[0] == 'missing' and len(fields) == 3:
            result['missingDependencies'].append({'capability': fields[1], 'package': fields[2]})
        elif fields[0] == 'log' and len(fields) == 2 and re.fullmatch(r'/var/log/gbf-reborn-dependencies/install\.[A-Za-z0-9]+', fields[1]):
            result['logPath'] = fields[1]
        elif fields[0] == 'changed' and len(fields) == 2 and fields[1] in ('true', 'false'):
            result['changed'] = fields[1] == 'true'
        elif line == 'ready': result['checksComplete'] = True
        else: raise ValueError('Invalid dependency report')
    if 'machineIdHash' not in result or not result['checksComplete']: raise ValueError('Incomplete dependency report')
    result['checksComplete'] = not result['missingDependencies']
    return result


def require_ready(reports):
    missing = [target for target, report in reports.items() if report['missingDependencies']]
    if missing: raise RuntimeError('遠端依賴尚未齊全：' + ', '.join(missing) + '；請先以同一配置執行 --install-deps。')


def prepare(ssh, reports):
    seen = {}
    for target, report in reports.items():
        identity = report['machineIdHash']
        if identity not in seen:
            seen[identity] = ssh.dependencies(target, install=True, identity=identity)
            if seen[identity]['machineIdHash'] != identity: raise RuntimeError('Host identity changed during dependency installation')
        if seen[identity]['architecture'] != report['architecture']: raise RuntimeError('Host architecture changed during dependency installation')
    result = {target: seen[report['machineIdHash']] for target, report in reports.items()}
    require_ready(result)
    return result
