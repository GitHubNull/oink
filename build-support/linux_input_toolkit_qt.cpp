// Qt native-callback fixture for dedicated-master input delivery tests.
// A QTextEdit subclass prints one bounded JSON line per native key/wheel
// callback so the harness can prove delivery through the Oink-Toolkit master.
// Refuses any environment the private runner did not create.
#include <QApplication>
#include <QTextEdit>
#include <QKeyEvent>
#include <QWheelEvent>
#include <csignal>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <unistd.h>

static void require_isolated() {
    const char *root = getenv("OINK_NATIVE_TEST_ROOT");
    const char *authority = getenv("XAUTHORITY");
    const char *display = getenv("DISPLAY");
    bool ok = root && strncmp(root, "/tmp/oink-native-", 17) == 0 && !strchr(root + 5, '/');
    ok = ok && authority && strncmp(authority, root, strlen(root)) == 0;
    ok = ok && display && display[0] == ':' && display[1] >= '1' && display[1] <= '9';
    if (!ok) {
        fprintf(stderr, "qt fixture: refused outside the private native runner\n");
        exit(2);
    }
}

class Reporter : public QTextEdit {
public:
    using QTextEdit::QTextEdit;
protected:
    void keyPressEvent(QKeyEvent *event) override {
        report("key", true, event);
        QTextEdit::keyPressEvent(event);
    }
    void keyReleaseEvent(QKeyEvent *event) override {
        report("key", false, event);
        QTextEdit::keyReleaseEvent(event);
    }
    void wheelEvent(QWheelEvent *event) override {
        printf("{\"type\":\"wheel\",\"dx\":%d,\"dy\":%d,\"mods\":%u}\n",
               event->angleDelta().x(), event->angleDelta().y(), (unsigned)event->modifiers());
        fflush(stdout);
        QTextEdit::wheelEvent(event);
    }
private:
    void report(const char *type, bool press, QKeyEvent *event) {
        printf("{\"type\":\"%s\",\"press\":%s,\"key\":%d,\"native_scan\":%u,\"native_virt\":%u,"
               "\"mods\":%u,\"auto_repeat\":%s}\n", type, press ? "true" : "false",
               event->key(), event->nativeScanCode(), event->nativeVirtualKey(),
               (unsigned)event->modifiers(), event->isAutoRepeat() ? "true" : "false");
        fflush(stdout);
    }
};

int main(int argc, char **argv) {
    require_isolated();
    struct sigaction action = {};
    action.sa_handler = [](int) { _exit(0); };
    sigemptyset(&action.sa_mask);
    sigaction(SIGTERM, &action, nullptr);
    QApplication app(argc, argv);
    app.setApplicationName("oink-toolkit-qt");
    app.setApplicationDisplayName("oink-toolkit-qt");
    Reporter reporter;
    reporter.setWindowTitle("oink-toolkit-qt");
    reporter.resize(640, 420);
    reporter.show();
    printf("{\"type\":\"fixture_ready\",\"xid\":%llu}\n", (unsigned long long)reporter.winId());
    fflush(stdout);
    return app.exec();
}
