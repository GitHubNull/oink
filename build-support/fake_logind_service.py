#!/usr/bin/env python3
"""私有总线上的假 logind 会话，仅供阶段 4 生命周期探针使用。

用法：fake_logind_service.py <总线地址> <状态文件>
状态文件内容为 0（解锁）或 1（锁定），属性每次读取都重新取文件，便于外部切换。
只实现 logind/auto 会话的 LockedHint/Locked 只读属性，不模拟任何写操作。
"""

import sys

from gi.repository import Gio, GLib

XML = '''
<node>
  <interface name="org.freedesktop.login1.Session">
    <property name="LockedHint" type="b" access="read"/>
    <property name="Locked" type="b" access="read"/>
  </interface>
</node>'''


def main() -> int:
    address, state_file = sys.argv[1], sys.argv[2]
    connection = Gio.DBusConnection.new_for_address_sync(
        address,
        Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT
        | Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION,
        None,
        None,
    )
    info = Gio.DBusNodeInfo.new_for_xml(XML)

    def read_locked(_connection, _sender, _path, _interface, _property):
        try:
            with open(state_file, encoding='ascii') as handle:
                locked = handle.read().strip() == '1'
        except OSError:
            locked = False
        return GLib.Variant('b', locked)

    connection.register_object(
        '/org/freedesktop/login1/session/auto',
        info.interfaces[0],
        None,
        read_locked,
        None,
    )
    reply = connection.call_sync(
        'org.freedesktop.DBus',
        '/org/freedesktop/DBus',
        'org.freedesktop.DBus',
        'RequestName',
        GLib.Variant('(su)', ('org.freedesktop.login1', 0)),
        None,
        Gio.DBusCallFlags.NONE,
        -1,
        None,
    )
    if reply.unpack()[0] != 1:
        print('name unavailable', file=sys.stderr, flush=True)
        return 1
    print('ready', flush=True)
    GLib.MainLoop().run()
    return 0


if __name__ == '__main__':
    sys.exit(main())
