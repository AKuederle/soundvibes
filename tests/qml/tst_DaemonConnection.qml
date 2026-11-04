import QtQuick
import QtTest
import "../../contrib/plasmoid/contents/ui" as Widget

TestCase {
    name: "PlasmoidDaemonConnection"
    when: windowShown
    Widget.DaemonConnection { id: backend }
    function test_live_dbus_connection() {
        tryCompare(backend, "connected", true, 5000);
        compare(backend.activity, "idle");
        compare(backend.recordings.length, 1);
        compare(backend.recordings[0].text, "Full_Unicode 十\nsecond line");
        backend.copyRecording("missing-id");
        tryVerify(() => backend.errorMessage.length > 0);
        const recordingId = backend.recordings[0].id;
        backend.copyRecording(recordingId);
        tryCompare(backend, "activity", "listening", 5000);
        backend.copyRecording(recordingId);
        tryCompare(backend, "connected", false, 5000);
        compare(backend.activity, "offline");
        compare(backend.recordings.length, 0);
    }
}
