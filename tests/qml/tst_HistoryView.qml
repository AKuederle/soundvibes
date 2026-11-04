import QtQuick
import QtTest
import "../../contrib/plasmoid/contents/ui" as Widget

TestCase {
    name: "PlasmoidHistory"
    when: windowShown
    visible: true
    width: 420
    height: 400
    Widget.HistoryView {
        id: history
        anchors.fill: parent
    }
    SignalSpy { id: copySpy; target: history; signalName: "copyRequested" }
    function test_wrapping_and_copy() {
        const text = "Full <literal markup> and Unicode 十. " + "Words to wrap. ".repeat(30) + "long_unbroken_token_".repeat(20);
        history.recordings = [{id: "recording-one", text: text}, {id: "recording-two", text: text}];
        const list = findChild(history, "recordingList");
        tryVerify(() => list.itemAtIndex(0) !== null);
        const row = list.itemAtIndex(0);
        const label = findChild(row, "recordingText");
        compare(label.text, text);
        verify(label.height > label.font.pixelSize * 3);
        verify(label.width < history.width);
        verify(!label.truncated);
        tryVerify(() => list.contentHeight > list.height);
        mouseClick(row, 10, 10);
        compare(copySpy.count, 1);
        compare(copySpy.signalArguments[0][0], "recording-one");
        compare(history.recordings[0].text, text);
    }
    function test_empty_history() {
        history.recordings = [];
        wait(0);
        compare(findChild(history, "recordingList").count, 0);
    }
}
