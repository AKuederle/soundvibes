#include <QApplication>
#include <QDialog>
#include <QCursor>
#include <QGuiApplication>
#include <QJsonArray>
#include <QJsonDocument>
#include <QJsonObject>
#include <QLabel>
#include <QPushButton>
#include <QScreen>
#include <QScrollArea>
#include <QScrollBar>
#include <QShortcut>
#include <QTimer>
#include <QVBoxLayout>
#include <cstdio>

// A normal, keyboard-accessible button whose label participates in Qt's
// height-for-width layout. Plain text prevents transcripts becoming markup.
class RecordingButton : public QPushButton {
public:
    QLabel *label;
    explicit RecordingButton(const QString &text, QWidget *parent) : QPushButton(parent) {
        label = new QLabel(text, this);
        label->setTextFormat(Qt::PlainText);
        label->setWordWrap(true);
        label->setAttribute(Qt::WA_TransparentForMouseEvents);
        label->setSizePolicy(QSizePolicy::Ignored, QSizePolicy::Preferred);
        setAccessibleName(text);
        setToolTip(QStringLiteral("Copy complete recording"));
        auto policy = QSizePolicy(QSizePolicy::Expanding, QSizePolicy::Preferred);
        policy.setHeightForWidth(true);
        setSizePolicy(policy);
    }
    bool hasHeightForWidth() const override { return true; }
    int heightForWidth(int width) const override {
        return label->heightForWidth(qMax(1, width - 28)) + 24;
    }
    QSize sizeHint() const override { return QSize(380, heightForWidth(width() > 28 ? width() : 380)); }
    void resizeEvent(QResizeEvent *event) override {
        QPushButton::resizeEvent(event);
        const int required = heightForWidth(width());
        setFixedHeight(required);
        label->setGeometry(14, 12, qMax(1, width() - 28), required - 24);
    }
    QSize minimumSizeHint() const override { return QSize(1, fontMetrics().height() + 24); }
};

extern "C" int sv_history_popup(const unsigned char *json, size_t len, bool inspect, int select) {
    const auto document = QJsonDocument::fromJson(QByteArray(reinterpret_cast<const char *>(json), qsizetype(len)));
    if (!document.isObject()) return -1;
    const auto request = document.object();
    int argc = 1;
    char name[] = "sv-history";
    char *argv[] = {name, nullptr};
    QApplication app(argc, argv);
    app.setApplicationName(QStringLiteral("SoundVibes"));
    // A tool window works as a standalone Wayland toplevel. Qt::Popup requires
    // a parent surface, which a tray-hosted icon in another process cannot supply.
    QDialog popup(nullptr, Qt::Tool);
    popup.setWindowTitle(QStringLiteral("SoundVibes recordings"));
    const QPoint anchor = request["x"].toInt() == 0 && request["y"].toInt() == 0
        ? QCursor::pos() : QPoint(request["x"].toInt(), request["y"].toInt());
    auto screen = QGuiApplication::screenAt(anchor);
    if (!screen) screen = QGuiApplication::primaryScreen();
    const QRect available = screen->availableGeometry();
    popup.setFixedWidth(qMin(420, available.width()));
    auto layout = new QVBoxLayout(&popup);
    layout->setContentsMargins(12, 12, 12, 12);
    layout->setSpacing(10);
    auto heading = new QLabel(QStringLiteral("Recent recordings"));
    auto headingFont = heading->font();
    headingFont.setBold(true);
    heading->setFont(headingFont);
    layout->addWidget(heading);
    auto status = new QLabel(request["title"].toString());
    status->setTextFormat(Qt::PlainText);
    layout->addWidget(status);
    auto hint = new QLabel(QStringLiteral("Click a recording to copy its full text."));
    hint->setWordWrap(true);
    layout->addWidget(hint);
    auto scroll = new QScrollArea;
    scroll->setFrameShape(QFrame::NoFrame);
    scroll->setWidgetResizable(true);
    scroll->setHorizontalScrollBarPolicy(Qt::ScrollBarAlwaysOff);
    auto content = new QWidget;
    auto rows = new QVBoxLayout(content);
    rows->setContentsMargins(0, 0, 0, 0);
    rows->setSpacing(8);
    rows->setSizeConstraint(QLayout::SetMinAndMaxSize);
    QList<RecordingButton *> buttons;
    int selected = -1;
    const auto transcripts = request["transcripts"].toArray();
    for (int i = 0; i < transcripts.size(); ++i) {
        auto button = new RecordingButton(transcripts[i].toString(), content);
        QObject::connect(button, &QPushButton::clicked, &popup, [&, i] {
            selected = i;
            popup.accept();
        });
        buttons.append(button);
        rows->addWidget(button);
    }
    if (buttons.isEmpty()) {
        auto empty = new QLabel(QStringLiteral("No recordings yet. Record something to see it here."));
        empty->setWordWrap(true);
        rows->addWidget(empty);
    }
    rows->addStretch();
    scroll->setWidget(content);
    layout->addWidget(scroll);
    new QShortcut(QKeySequence(Qt::Key_Escape), &popup, [&] { popup.reject(); });
    bool wasActive = false;
    QObject::connect(&app, &QGuiApplication::applicationStateChanged, &popup, [&](Qt::ApplicationState state) {
        if (state == Qt::ApplicationActive) wasActive = true;
        else if (wasActive && !inspect) popup.reject();
    });
    const int bodyHeight = qMax(48, rows->heightForWidth(popup.width() - 24));
    const int headerHeight = 24 + heading->sizeHint().height() + status->sizeHint().height()
        + hint->heightForWidth(popup.width() - 24) + layout->spacing() * 3;
    popup.resize(popup.width(), qMin(qMin(560, available.height()), headerHeight + bodyHeight));
    popup.move(qBound(available.left(), anchor.x(), available.right() - popup.width() + 1),
               qBound(available.top(), anchor.y() - popup.height(), available.bottom() - popup.height() + 1));
    if (!buttons.isEmpty()) buttons.first()->setFocus();
#ifdef SV_TEST_SUPPORT
    if (inspect) {
        QTimer::singleShot(0, &popup, [&] {
            QApplication::processEvents();
            QJsonArray measurements;
            for (auto button : buttons) {
                measurements.append(QJsonObject{
                    {"text", button->label->text()}, {"width", button->width()},
                    {"height", button->height()}, {"line_height", button->fontMetrics().height()},
                    {"label_height", button->label->height()}, {"label_width", button->label->width()},
                    {"label_visible", button->label->isVisible()},
                    {"label_required_height", button->label->heightForWidth(button->label->width())},
                    {"required_height", button->heightForWidth(button->width())}});
            }
            const QString screenshot = qEnvironmentVariable("SV_POPUP_SCREENSHOT");
            if (!screenshot.isEmpty()) popup.grab().save(screenshot);
            if (select >= 0 && select < buttons.size()) buttons[select]->click();
            const auto report = QJsonDocument(QJsonObject{
                {"width", popup.width()}, {"height", popup.height()}, {"rows", measurements},
                {"horizontal_scroll_max", scroll->horizontalScrollBar()->maximum()},
                {"vertical_scroll_max", scroll->verticalScrollBar()->maximum()}, {"selected", selected}}).toJson(QJsonDocument::Compact);
            std::fwrite(report.constData(), 1, size_t(report.size()), stdout);
            popup.reject();
        });
    }
#else
    Q_UNUSED(inspect);
    Q_UNUSED(select);
#endif
    popup.exec();
    return selected + 1;
}
