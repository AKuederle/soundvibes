pragma ComponentBehavior: Bound
import QtQuick
import QtQuick.Layouts
import org.kde.plasma.plasmoid
import org.kde.plasma.core as PlasmaCore
import org.kde.plasma.extras as PlasmaExtras
import org.kde.plasma.components as PlasmaComponents
import org.kde.kirigami as Kirigami

PlasmoidItem {
    id: root
    readonly property string activity: backend.activity
    readonly property string stateText: activity === "listening" ? qsTr("Listening")
        : activity === "processing" ? qsTr("Processing")
        : activity === "offline" ? qsTr("Not running") : qsTr("Idle")
    Plasmoid.status: PlasmaCore.Types.ActiveStatus
    Plasmoid.icon: Qt.resolvedUrl("../images/" + (activity === "offline" ? "idle" : activity) + ".svg")
    toolTipMainText: "SoundVibes: " + stateText
    toolTipSubText: qsTr("Click to view and copy complete recordings")
    toolTipTextFormat: Text.PlainText
    switchWidth: Kirigami.Units.gridUnit * 12
    switchHeight: Kirigami.Units.gridUnit * 12

    DaemonConnection {
        id: backend
        onCopied: root.expanded = false
    }
    compactRepresentation: MouseArea {
        implicitWidth: Kirigami.Units.iconSizes.smallMedium
        implicitHeight: implicitWidth
        activeFocusOnTab: true
        Accessible.name: root.toolTipMainText
        Accessible.role: Accessible.Button
        Accessible.onPressAction: root.expanded = !root.expanded
        onClicked: root.expanded = !root.expanded
        Keys.onReturnPressed: root.expanded = !root.expanded
        Keys.onSpacePressed: root.expanded = !root.expanded
        Rectangle {
            anchors.centerIn: parent
            width: Math.min(parent.width, parent.height) * 0.7
            height: width
            radius: width / 2
            color: root.activity === "listening" ? "#eb2d32" : root.activity === "processing" ? "#f5aa23" : "#919191"
            opacity: backend.connected ? 1 : 0.45
        }
    }
    fullRepresentation: PlasmaExtras.Representation {
        Layout.minimumWidth: Kirigami.Units.gridUnit * 18
        Layout.preferredWidth: Kirigami.Units.gridUnit * 24
        Layout.maximumWidth: Kirigami.Units.gridUnit * 40
        Layout.minimumHeight: Kirigami.Units.gridUnit * 12
        Layout.preferredHeight: Kirigami.Units.gridUnit * 26
        Layout.maximumHeight: Kirigami.Units.gridUnit * 40
        collapseMarginsHint: true
        header: PlasmaExtras.BasicPlasmoidHeading {
            contentItem: PlasmaComponents.Label {
                text: qsTr("Recent recordings")
                font.bold: true
            }
        }
        contentItem: HistoryView {
            recordings: backend.recordings
            connected: backend.connected
            onCopyRequested: recordingId => backend.copyRecording(recordingId)
        }
        footer: PlasmaComponents.Label {
            text: backend.errorMessage
            visible: text.length > 0
            wrapMode: Text.Wrap
            textFormat: Text.PlainText
            color: Kirigami.Theme.negativeTextColor
        }
    }
}
