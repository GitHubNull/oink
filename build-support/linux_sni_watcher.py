#!/usr/bin/env python3
"""Minimal StatusNotifierWatcher for private Linux tray probes.

Owns org.kde.StatusNotifierWatcher on an isolated session bus and records the
items registered by libayatana-appindicator into a JSON state file. The probe
reads that file and drives the item's com.canonical.dbusmenu directly, so the
menu still travels through the product's real appindicator export.
"""
import argparse
import json
import os
import signal
import sys
from pathlib import Path

import dbus
import dbus.service
from dbus.mainloop.glib import DBusGMainLoop
from gi.repository import GLib

BUS_NAME = 'org.kde.StatusNotifierWatcher'
OBJECT_PATH = '/StatusNotifierWatcher'
IFACE = 'org.kde.StatusNotifierWatcher'


def atomic_write(path, value):
    temporary = path.with_name(path.name + '.tmp')
    temporary.write_text(json.dumps(value, indent=2, sort_keys=True))
    os.replace(temporary, path)


class Watcher(dbus.service.Object):
    def __init__(self, bus, state_path):
        super().__init__(bus, OBJECT_PATH)
        self.state_path = state_path
        self.items = []
        self.hosts = []
        self.persist()

    def persist(self):
        atomic_write(self.state_path, {
            'ready': True,
            'items': self.items,
            'hosts': self.hosts,
            'is_host_registered': True,
            'protocol_version': 0,
        })

    @dbus.service.method(IFACE, in_signature='s', sender_keyword='sender')
    def RegisterStatusNotifierItem(self, service, sender=None):
        entry = {'service': str(service), 'sender': str(sender or '')}
        if entry not in self.items:
            self.items.append(entry)
            self.persist()
            print('registered_item=' + json.dumps(entry, sort_keys=True), flush=True)
            self.RegisteredStatusNotifierItem(entry['service'])

    @dbus.service.method(IFACE, in_signature='s', sender_keyword='sender')
    def RegisterStatusNotifierHost(self, service, sender=None):
        entry = {'service': str(service), 'sender': str(sender or '')}
        if entry not in self.hosts:
            self.hosts.append(entry)
            self.persist()
            print('registered_host=' + json.dumps(entry, sort_keys=True), flush=True)
            self.StatusNotifierHostRegistered()

    @dbus.service.method(IFACE, in_signature='', out_signature='b')
    def IsStatusNotifierHostRegistered(self):
        return True

    @dbus.service.method(IFACE, in_signature='', out_signature='i')
    def GetProtocolVersion(self):
        return 0

    def property_value(self, name):
        if name == 'RegisteredStatusNotifierItems':
            return dbus.Array([item['service'] for item in self.items], signature='s')
        if name == 'IsStatusNotifierHostRegistered':
            return dbus.Boolean(True)
        if name == 'ProtocolVersion':
            return dbus.Int32(0)
        return None

    @dbus.service.method('org.freedesktop.DBus.Properties', in_signature='ss', out_signature='v')
    def Get(self, interface, prop):
        value = self.property_value(prop)
        if value is None:
            raise dbus.exceptions.DBusException(
                'Unknown property ' + prop,
                name='org.freedesktop.DBus.Error.UnknownProperty')
        return value

    @dbus.service.method('org.freedesktop.DBus.Properties', in_signature='s', out_signature='a{sv}')
    def GetAll(self, interface):
        return dbus.Dictionary({
            'RegisteredStatusNotifierItems': self.property_value('RegisteredStatusNotifierItems'),
            'IsStatusNotifierHostRegistered': dbus.Boolean(True),
            'ProtocolVersion': dbus.Int32(0),
        }, signature='sv')

    @dbus.service.signal(IFACE, signature='s')
    def RegisteredStatusNotifierItem(self, service):
        pass

    @dbus.service.signal(IFACE, signature='')
    def StatusNotifierHostRegistered(self):
        pass

    @dbus.service.signal(IFACE, signature='')
    def StatusNotifierHostUnregistered(self):
        pass


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--state', required=True)
    args = parser.parse_args()
    state_path = Path(args.state)
    state_path.parent.mkdir(parents=True, exist_ok=True)
    DBusGMainLoop(set_as_default=True)
    bus = dbus.SessionBus()
    name = dbus.service.BusName(BUS_NAME, bus)
    watcher = Watcher(bus, state_path)
    loop = GLib.MainLoop()
    signal.signal(signal.SIGTERM, lambda *_: loop.quit())
    signal.signal(signal.SIGINT, lambda *_: loop.quit())
    print('sni_watcher_ready=' + json.dumps({
        'name': BUS_NAME,
        'address': os.environ.get('DBUS_SESSION_BUS_ADDRESS', ''),
        'state': str(state_path),
        'bus_name_owned': name.get_name() == BUS_NAME,
    }, sort_keys=True), flush=True)
    loop.run()
    watcher.remove_from_connection()
    return 0


if __name__ == '__main__':
    sys.exit(main())
