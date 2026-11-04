import QtQuick
import org.kde.plasma.workspace.dbus as DBus

QtObject {
    id: root
    property string serviceName: "org.soundvibes.Daemon"
    property string errorMessage: ""
    property bool ready: false
    readonly property string snapshotJson: String(remote.properties.Snapshot ?? "")
    readonly property bool connected: watcher.registered && ready && snapshotJson.length > 0
    readonly property var snapshot: connected ? JSON.parse(snapshotJson) : ({activity: "offline", recordings: []})
    readonly property string activity: snapshot.activity
    readonly property var recordings: snapshot.recordings
    signal copied()

    property DBus.DBusServiceWatcher watcher: DBus.DBusServiceWatcher {
        busType: DBus.BusType.Session
        watchedService: root.serviceName
        onRegisteredChanged: {
            root.ready = false;
            if (registered) root.remote.updateAll();
        }
    }
    property DBus.Properties remote: DBus.Properties {
        busType: DBus.BusType.Session
        service: root.serviceName
        path: "/org/soundvibes/Daemon"
        iface: "org.soundvibes.Daemon"
        onRefreshed: root.ready = true
    }
    function copyRecording(recordingId: string): void {
        errorMessage = "";
        DBus.SessionBus.asyncCall({
            service: serviceName,
            path: "/org/soundvibes/Daemon",
            iface: "org.soundvibes.Daemon",
            member: "CopyRecording",
            arguments: [recordingId]
        }, function(reply) {
            root.copied();
        }, function(reply) {
            root.errorMessage = qsTr("Could not copy recording: %1").arg(reply.error.message);
        });
    }
}
