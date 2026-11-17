#include "linux_dnd_qt.h"
#include <X11/Xatom.h>
#include <csignal>

extern "C" {
struct Report r = {};
bool is_source;
Atom selection_atom;
}
QtRole *role;
static int identityError;

extern "C" void require(bool condition, const char *message) {
    if (condition) return;
    fprintf(stderr, "Qt role FAIL: %s (generation=%lu)\n", message, r.gen);
    exit(1);
}

static int recordIdentityError(Display *, XErrorEvent *error) {
    identityError = error->error_code;
    return 0;
}

bool QtRole::identity() const {
    if (!r.armed || !incoming || incoming != expected
            || XGetSelectionOwner(inject, selection_atom) != expected) return false;
    Atom type;
    int format;
    unsigned long count, remaining;
    unsigned char *data = nullptr;
    XSync(inject, False);
    identityError = 0;
    auto previous = XSetErrorHandler(recordIdentityError);
    int status = XGetWindowProperty(inject, expected, generationAtom, 0, 1, False,
                                   XA_CARDINAL, &type, &format, &count, &remaining, &data);
    XSync(inject, False);
    XSetErrorHandler(previous);
    bool valid = !identityError && status == Success && type == XA_CARDINAL
                 && format == 32 && count == 1 && !remaining && data
                 && *reinterpret_cast<unsigned long *>(data) == r.gen;
    if (data) XFree(data);
    return valid;
}

extern "C" void toolkit_poll(void) {
    r.toolkit_grabbed = role->active;
    if (is_source && role->active && r.owner && !r.source_xid) {
        r.source_xid = r.owner;
        unsigned long gen = r.gen;
        XChangeProperty(inject, r.source_xid, role->generationAtom, XA_CARDINAL, 32,
                        PropModeReplace, reinterpret_cast<unsigned char *>(&gen), 1);
        XSync(inject, False);
    }
}

extern "C" void toolkit_held(int pointer_id) {
    (void)pointer_id;
    require(role->active && role->drag && role->drag->source() == role,
            "original QDrag object remains active in exec");
}

extern "C" void toolkit_released(void) {
    require(!role->active, "QDrag exec has returned");
}

QtRole::QtRole() {
    selection_atom = XInternAtom(inject, "XdndSelection", False);
    generationAtom = XInternAtom(inject, "_OINK_TEST_DND_GENERATION", False);
    enterAtom = XInternAtom(inject, "XdndEnter", False);
    positionAtom = XInternAtom(inject, "XdndPosition", False);
    dropAtom = XInternAtom(inject, "XdndDrop", False);
    statusAtom = XInternAtom(inject, "XdndStatus", False);
    finishedAtom = XInternAtom(inject, "XdndFinished", False);
    copyAtom = XInternAtom(inject, "XdndActionCopy", False);
    utf8Atom = XInternAtom(inject, "UTF8_STRING", False);
    clipboardAtom = XInternAtom(inject, "CLIPBOARD", False);
    setWindowFlags(Qt::Window | Qt::FramelessWindowHint);
    resize(230, 220);
    move(is_source ? 100 : 900, 220);
    setWindowTitle(is_source ? "私有 Qt 拖拽源" : "私有 Qt 拖拽目标");
    setStyleSheet(is_source ? "background: #b5d9f6" : "background: #bde6ca");
    setAcceptDrops(!is_source);
    if (is_source) {
        for (int i = 0; i < 48; i++)
            body.append(static_cast<char>(QRandomGenerator::system()->bounded(33, 127)));
        payloadReport();
    }
}

void QtRole::geometryReport() {
    if (!isVisible()) return;
    QPoint center = mapToGlobal(rect().center());
    require(center.x() >= 0 && center.y() >= 0, "private visible Qt geometry");
    r.xid = winId(); r.x = center.x(); r.y = center.y();
}

void QtRole::clearPayload() {
    body.fill('\0'); body.clear(); payloadReport();
}

void QtRole::payloadReport() {
    r.length = static_cast<unsigned long>(body.size());
    uint32_t hash = body.isEmpty() ? 0 : 2166136261u;
    for (unsigned char byte : body) hash = (hash ^ byte) * 16777619u;
    r.hash = hash;
    QByteArray digest = body.isEmpty() ? QByteArray(32, '\0')
                                     : QCryptographicHash::hash(body, QCryptographicHash::Sha256);
    unsigned long *words[] = {&r.digest0, &r.digest1, &r.digest2, &r.digest3,
                             &r.digest4, &r.digest5, &r.digest6, &r.digest7};
    for (unsigned i = 0; i < 8; i++) {
        *words[i] = 0;
        for (unsigned j = 0; j < 4; j++)
            *words[i] = (*words[i] << 8) | static_cast<unsigned char>(digest[i * 4 + j]);
    }
}

void QtRole::mousePressEvent(QMouseEvent *event) {
    if (is_source && event->button() == Qt::LeftButton) {
        require(event->spontaneous(), "real Qt button event");
        r.press++; pressPosition = event->position().toPoint();
    }
}

void QtRole::mouseMoveEvent(QMouseEvent *event) {
    if (!is_source || active || r.begin || !(event->buttons() & Qt::LeftButton)
            || (event->position().toPoint() - pressPosition).manhattanLength()
                < QApplication::startDragDistance()) return;
    require(event->spontaneous() && r.press == 1 && r.down == 1 && !r.up && !body.isEmpty(),
            "one QDrag from native held button");
    drag = new QDrag(this);
    auto *mime = new QMimeData;
    mime->setData("text/plain", body);
    drag->setMimeData(mime);
    QObject::connect(drag, &QDrag::actionChanged, this, [](Qt::DropAction action) {
        r.selected = action == Qt::CopyAction ? 2 : 0;
    });
    active = true; r.begin++;
    Qt::DropAction result = drag->exec(Qt::CopyAction, Qt::CopyAction);
    active = false;
    require(result == Qt::CopyAction || (result == Qt::IgnoreAction && cancelling),
            "QDrag returns COPY or requested Escape");
    if (result == Qt::IgnoreAction) { r.end++; r.failed++; r.cancelled++; }
    else if (copyFinished) r.end++;
    // Qt owns the pending transfer after exec returns, until native XdndFinished.

}

bool QtRole::nativeEventFilter(const QByteArray &type, void *message, qintptr *) {
    require(type == "xcb_generic_event_t", "Qt xcb native events only");
    auto *event = static_cast<xcb_generic_event_t *>(message);
    unsigned kind = event->response_type & 0x7f;
    if (is_source && kind == XCB_SELECTION_REQUEST) {
        auto *request = reinterpret_cast<xcb_selection_request_event_t *>(event);
        if (request->selection == selection_atom && request->target == utf8Atom) {
            native_poll();
            require(r.begin == 1 && !cancelling && !copyFinished && drag && r.source_xid
                    && r.owner == r.source_xid && request->owner == r.source_xid && !r.end,
                    "live native XdndSelection UTF8_STRING request");
            require(drag->mimeData()->data("text/plain") == body, "source payload remains intact");
            r.get++;
        }
    }
    if (is_source && kind == XCB_CLIENT_MESSAGE) {
        auto *client = reinterpret_cast<xcb_client_message_event_t *>(event);
        if (client->window == r.source_xid && client->format == 32) {
            if (client->type == statusAtom && active) statusTarget = client->data.data32[0];
            if (client->type == finishedAtom) {
                require(!cancelling && !copyFinished && !r.end && r.up == 1 && r.get > 0
                        && statusTarget && client->data.data32[0] == statusTarget
                        && (client->data.data32[1] & 1) && client->data.data32[2] == copyAtom,
                        "one native COPY XdndFinished from the selected target");
                require(drag && drag->mimeData()->data("text/plain") == body,
                        "native COPY preserves original source payload");
                copyFinished = true;
                if (!active) r.end++;
            }
        }
    }
    if (!is_source && kind == XCB_CLIENT_MESSAGE) {
        auto *client = reinterpret_cast<xcb_client_message_event_t *>(event);
        if (client->window == winId() && client->format == 32) {
            if (client->type == enterAtom) incoming = client->data.data32[0];
            if (client->type == enterAtom || client->type == positionAtom || client->type == dropAtom) {
                if (!identity() || client->data.data32[0] != expected) return true;
                r.source_xid = incoming;
            }
        }
    }
    return false;
}

void QtRole::dragEnterEvent(QDragEnterEvent *event) {
    if (!identity() || !event->mimeData()->hasFormat("text/plain")
            || !(event->possibleActions() & Qt::CopyAction)) { event->ignore(); return; }
    if (!r.inside) { r.inside = 1; r.visits++; }
    event->setDropAction(Qt::CopyAction); event->accept();
}

void QtRole::dragMoveEvent(QDragMoveEvent *event) {
    if (!identity()) { event->ignore(); return; }
    r.motions++;
    event->setDropAction(Qt::CopyAction); event->accept();
}

void QtRole::dragLeaveEvent(QDragLeaveEvent *event) {
    r.inside = 0; r.leaves++; event->accept();
}

void QtRole::dropEvent(QDropEvent *event) {
    require(identity() && !r.pending && ++r.drop == 1
            && (event->possibleActions() & Qt::CopyAction), "armed Qt COPY drop");
    unsigned long generation = r.gen, epoch = r.epoch;
    r.pending = 1;
    QByteArray received = event->mimeData()->data("text/plain");
    r.pending = 0;
    if (generation != r.gen || epoch != r.epoch || !identity()) {
        r.stale++; event->ignore(); return;
    }
    require(!received.isEmpty() && received.size() <= PAYLOAD_MAX, "bounded native Qt selection");
    body = received; payloadReport(); r.received++;
    event->setDropAction(Qt::CopyAction); event->accept();
}

void QtRole::stop() {
    if (active) { cancelling = true; QDrag::cancel(); }
    if (is_source && r.down && !r.up) button(false);
    QTimer::singleShot(0, qApp, &QCoreApplication::quit);
}

int main(int argc, char **argv) {
    require_isolated();
    require(argc == 2 && (!strcmp(argv[1], "source") || !strcmp(argv[1], "target")), "Qt source/target role");
    is_source = !strcmp(argv[1], "source"); r.gen = 1;
    alarm(90); signal(SIGPIPE, SIG_IGN);
    QApplication app(argc, argv);
    require(QApplication::platformName() == "xcb", "Qt xcb backend required");
    native_init();
    QtRole widget; role = &widget;
    app.installNativeEventFilter(role);
    widget.show();
    qtControlStart();
    int result = app.exec();
    qtFlush();
    app.removeNativeEventFilter(role);
    XCloseDisplay(observer); XCloseDisplay(inject);
    return result;
}
