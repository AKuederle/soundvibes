pragma ComponentBehavior: Bound
import QtQuick
import org.kde.plasma.components as PlasmaComponents
import org.kde.kirigami as Kirigami

PlasmaComponents.ScrollView {
    id: root
    property var recordings: []
    property bool connected: true
    signal copyRequested(string recordingId)
    clip: true

    contentItem: ListView {
        id: list
        objectName: "recordingList"
        clip: true
        model: root.recordings
        spacing: Kirigami.Units.smallSpacing
        boundsBehavior: Flickable.StopAtBounds
        currentIndex: -1
        delegate: PlasmaComponents.ItemDelegate {
            id: delegate
            required property var modelData
            width: list.width
            text: modelData.text
            activeFocusOnTab: true
            Accessible.name: text
            contentItem: PlasmaComponents.Label {
                objectName: "recordingText"
                text: delegate.text
                textFormat: Text.PlainText
                wrapMode: Text.Wrap
            }
            onClicked: root.copyRequested(modelData.id)
        }
    }
    Kirigami.PlaceholderMessage {
        anchors.centerIn: parent
        width: parent.width - Kirigami.Units.gridUnit * 2
        visible: root.recordings.length === 0
        text: root.connected ? qsTr("No recordings yet") : qsTr("SoundVibes is not running")
        explanation: root.connected ? qsTr("Completed recordings will appear here. Click one to copy its full text.") : qsTr("Start the SoundVibes daemon to record and view transcripts.")
    }
}
