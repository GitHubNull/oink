#include "linux_dnd_qt.h"
#include <cerrno>
#include <fcntl.h>

static QByteArray output;

void qtFlush() {
    if (output.isEmpty()) return;
    ssize_t count = write(STDOUT_FILENO, output.constData(), static_cast<size_t>(output.size()));
    if (count < 0) {
        require(errno == EAGAIN || errno == EWOULDBLOCK || errno == EINTR, "Qt control output disconnected");
        return;
    }
    output.remove(0, count);
}

void qtReport(unsigned long sequence, unsigned long status) {
    role->geometryReport(); native_poll();
    QByteArray line = "R " + QByteArray::number(sequence) + ' ' + QByteArray::number(status);
#define PRINT(name) line += ' ' + QByteArray::number(r.name);
    FIELDS(PRINT)
#undef PRINT
    line += '\n';
    require(output.size() + line.size() <= 16384, "bounded Qt output backpressure");
    output += line; qtFlush();
}

unsigned long qtDispatch(const unsigned long *v, size_t count) {
    unsigned long op = v[1], gen = v[2];
    const unsigned long *a = v + 3;
    size_t argc = count - 3;
    if (op == ARM && !is_source && argc == 1 && a[0] && gen >= r.gen) {
        r.gen = gen; role->expected = a[0]; r.armed = 1; r.epoch++;
        r.inside = 0; role->clearPayload(); return 0;
    }
    if (gen != r.gen) return 1;
    switch (op) {
    case STATUS: return argc ? 2 : 0;
    case FOCUS:
        if (!is_source || argc || r.down) return 2;
        XSetInputFocus(inject, r.xid, RevertToParent, CurrentTime);
        motion(static_cast<int>(r.x), static_cast<int>(r.y)); return 0;
    case MOVE:
        if (!is_source || argc != 2 || a[0] > 32767 || a[1] > 32767) return 2;
        motion(static_cast<int>(a[0]), static_cast<int>(a[1])); return 0;
    case DOWN:
        if (!is_source || argc || r.down || role->body.isEmpty()) return 2;
        button(true); return 0;
    case UP:
        if (!is_source || argc || r.down != 1 || r.up) return 2;
        button(false); return 0;
    case ESCAPE:
        if (!is_source || argc || !role->active || role->cancelling) return 2;
        role->cancelling = true; escape_key(); return 0;
    case HELD:
    case RETURNED:
        if (!is_source || argc != 1) return 2;
        held_context(a[0]);
        if (op == RETURNED) { pointer_at_source(); r.returned++; }
        return 0;
    case FREE:
        if (argc) return 2;
        no_grab(); return 0;
    case CLIP_CHECK: {
        if (argc != 1 || !a[0]) return 2;
        Window before = XGetSelectionOwner(inject, role->clipboardAtom);
        QString text = QApplication::clipboard()->text(QClipboard::Clipboard);
        Window after = XGetSelectionOwner(inject, role->clipboardAtom);
        if (before != a[0] || after != a[0] || text != "oink-private-clipboard-sentinel") r.clip_bad++;
        r.clip_checks++; return 0;
    }
    case DISARM:
        if (is_source || argc) return 2;
        r.armed = 0; r.epoch++; r.inside = 0; role->clearPayload(); return 0;
    case QUIT:
        if (argc) return 2;
        role->stop(); return 0;
    default: return 2;
    }
}

static void command(const QByteArray &line) {
    unsigned long values[PAYLOAD_MAX + 4];
    size_t count = 0;
    for (const QByteArray &token : line.split(' ')) {
        require(count < sizeof(values) / sizeof(values[0]) && !token.isEmpty() && token.size() <= 10,
                "bounded Qt numeric control tokens");
        uint64_t value = 0;
        for (unsigned char byte : token) {
            require(byte >= '0' && byte <= '9', "Qt decimal control token");
            value = value * 10 + byte - '0';
        }
        require(value <= UINT32_MAX, "Qt uint32 control token");
        values[count++] = static_cast<unsigned long>(value);
    }
    require(count >= 3 && values[0] && values[2], "Qt sequence/opcode/generation required");
    qtReport(values[0], qtDispatch(values, count));
}

void qtControlStart() {
    require(fcntl(STDIN_FILENO, F_SETFL, O_NONBLOCK) == 0
            && fcntl(STDOUT_FILENO, F_SETFL, O_NONBLOCK) == 0, "nonblocking Qt control pipes");
    auto *input = new QSocketNotifier(STDIN_FILENO, QSocketNotifier::Read, qApp);
    QObject::connect(input, &QSocketNotifier::activated, qApp, [input] {
        static QByteArray line;
        char bytes[1024];
        ssize_t count = read(STDIN_FILENO, bytes, sizeof(bytes));
        if (count == 0) { input->setEnabled(false); role->stop(); return; }
        if (count < 0) {
            require(errno == EAGAIN || errno == EWOULDBLOCK || errno == EINTR, "Qt control read");
            return;
        }
        for (ssize_t i = 0; i < count; i++) {
            if (bytes[i] == '\n') {
                QByteArray complete = line; line.clear(); command(complete);
            } else {
                require(line.size() + 1 < LINE_MAXIMUM && (bytes[i] == ' '
                        || (bytes[i] >= '0' && bytes[i] <= '9')), "bounded ASCII Qt control frame");
                line += bytes[i];
            }
        }
    });
    auto *timer = new QTimer(qApp);
    QObject::connect(timer, &QTimer::timeout, qApp, [] {
        static bool ready;
        role->geometryReport(); native_poll(); qtFlush();
        if (!ready && r.xid) { ready = true; qtReport(0, 0); }
    });
    timer->start(10);
}
