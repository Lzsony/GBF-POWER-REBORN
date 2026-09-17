#!/usr/bin/python3
# GPR-MANAGED-SERVICE-CLI-v1
"""GBF POWER REBORN 服務管理；在已部署的 Linux 主機執行。"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import sys

INSTALL_PATH = Path('/usr/local/bin/gpr')
BASE = Path('/etc/gbf-reborn')
LIB = Path('/opt/gbf-reborn')
REGISTRY = Path('/var/lib/gbf-reborn-deploy/state.json')
ROLES = ('control', 'gateway')
SYSTEMCTL = '/usr/bin/systemctl'


def parser():
    result = argparse.ArgumentParser(prog='gpr', description=__doc__,
        epilog='範例：gpr admin；gpr status；gpr logs gateway -f；gpr restart gateway')
    commands = result.add_subparsers(dest='command')
    commands.add_parser('admin', help='開啟 Control 管理介面')
    for name, help_text in [('status', '查看服務狀態'), ('logs', '查看最近 100 行日誌'), ('doctor', '執行服務診斷')]:
        command = commands.add_parser(name, help=help_text)
        command.add_argument('role', nargs='?', default='all', choices=(*ROLES, 'all'))
        if name == 'logs': command.add_argument('-f', '--follow', action='store_true', help='持續追蹤日誌；Ctrl+C 結束')
    for name, help_text in [('start', '啟動服務'), ('stop', '停止服務'), ('restart', '重啟服務')]:
        command = commands.add_parser(name, help=help_text)
        command.add_argument('role', choices=(*ROLES, 'all'))
    return result


def installed_roles():
    registry = json.loads(REGISTRY.read_text()) if REGISTRY.exists() else {}
    return [role for role in ROLES if registry.get(role, {}).get('installed') is True
            and (BASE / role / 'config.json').is_file()
            and (LIB / role / 'current/reborn').is_file()]


def service_command(role, command):
    args = ['/usr/sbin/runuser', '-u', 'gbf-' + role, '--', str(LIB / role / 'current/reborn'), command]
    if command == 'admin': return args + ['--socket', '/run/gbf-control/admin.sock']
    return args + ['--role', role, '--config', str(BASE / role / 'config.json')]


def execute(args, roles):
    if args.command == 'admin':
        return subprocess.run(service_command('control', 'admin')).returncode
    if args.command == 'logs':
        command = ['/usr/bin/journalctl', '--no-pager', '-n', '100']
        for role in roles: command += ['-u', 'gbf-' + role + '.service']
        if args.follow: command.append('-f')
        return subprocess.run(command).returncode
    if args.command == 'doctor':
        failed = False
        labels = {'passed': '通過', 'failed': '失敗', 'not_checked': '未檢查'}
        for role in roles:
            print(f'[{role}]', flush=True)
            result = subprocess.run(service_command(role, 'doctor'), capture_output=True, text=True)
            if result.returncode:
                print('失敗：無法執行診斷；請檢查服務狀態與日誌。', file=sys.stderr)
                failed = True
                continue
            try:
                checks = json.loads(result.stdout)
                if not isinstance(checks, list) or not checks: raise ValueError('Empty diagnostics')
                for check in checks:
                    status = check['status']
                    if status not in labels: raise ValueError('Unknown diagnostic status')
                    print(f"{labels[status]}  {check['check']}: {check['detail']}")
                    failed |= status == 'failed'
            except (ValueError, KeyError, TypeError):
                print('失敗：無法讀取診斷結果。', file=sys.stderr)
                failed = True
        return int(failed)
    if args.command == 'status':
        return subprocess.run([SYSTEMCTL, '--no-pager', '--full', 'status',
            *['gbf-' + role + '.service' for role in roles]]).returncode
    # Restart all stops dependants first, then brings Control up before Gateway.
    actions = ('stop', 'start') if args.command == 'restart' and args.role == 'all' else (args.command,)
    for action in actions:
        for role in reversed(roles) if action == 'stop' else roles:
            result = subprocess.run([SYSTEMCTL, action, 'gbf-' + role + '.service'])
            if result.returncode: return result.returncode
    return 0


def main(argv=None):
    argv = sys.argv[1:] if argv is None else argv
    cli = parser()
    args = cli.parse_args(argv)
    if args.command is None:
        cli.print_help()
        return 0
    if os.geteuid() != 0:
        result = subprocess.run(['/usr/bin/sudo', '-n', '--', str(INSTALL_PATH), *argv])
        if result.returncode:
            print('管理操作未完成；請確認命令結果及管理帳號的 sudo -n 權限。', file=sys.stderr)
        return result.returncode
    available = installed_roles()
    requested = 'control' if args.command == 'admin' else args.role
    if requested != 'all' and requested not in available:
        print(f'此主機未安裝 {requested}；admin 須在 Control 主機執行。' if args.command == 'admin'
              else f'此主機未安裝 {requested}。', file=sys.stderr)
        return 1
    roles = available if requested == 'all' else [requested]
    if not roles:
        print('此主機尚未安裝可管理的服務。', file=sys.stderr)
        return 1
    return execute(args, roles)


if __name__ == '__main__':
    try: sys.exit(main())
    except KeyboardInterrupt: sys.exit(130)
    except (OSError, ValueError) as error:
        print(f'gpr: {error}', file=sys.stderr)
        sys.exit(1)
