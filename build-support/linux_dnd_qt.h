#ifndef OINK_LINUX_DND_QT_H
#define OINK_LINUX_DND_QT_H
#include <QAbstractNativeEventFilter>
#include <QApplication>
#include <QByteArray>
#include <QClipboard>
#include <QCryptographicHash>
#include <QDrag>
#include <QDragEnterEvent>
#include <QDropEvent>
#include <QMimeData>
#include <QMouseEvent>
#include <QPointer>
#include <QRandomGenerator>
#include <QSocketNotifier>
#include <QTimer>
#include <QWidget>
#include <xcb/xcb.h>
extern "C" {
#include "linux_dnd_native.h"
}

class QtRole final : public QWidget, public QAbstractNativeEventFilter {
public:
    QtRole();
    bool nativeEventFilter(const QByteArray &, void *, qintptr *) override;
    bool identity() const;
    void geometryReport();
    void clearPayload();
    void payloadReport();
    void stop();
    QByteArray body;
    QPointer<QDrag> drag;
    bool active = false, cancelling = false, copyFinished = false;
    Window expected = 0, incoming = 0, statusTarget = 0;
    Atom statusAtom = 0, finishedAtom = 0, copyAtom = 0;
    Atom generationAtom = 0, enterAtom = 0, positionAtom = 0, dropAtom = 0;
    Atom utf8Atom = 0, clipboardAtom = 0;
protected:
    void mousePressEvent(QMouseEvent *) override;
    void mouseMoveEvent(QMouseEvent *) override;
    void dragEnterEvent(QDragEnterEvent *) override;
    void dragMoveEvent(QDragMoveEvent *) override;
    void dragLeaveEvent(QDragLeaveEvent *) override;
    void dropEvent(QDropEvent *) override;
private:
    QPoint pressPosition;
};
extern QtRole *role;
void qtControlStart();
void qtReport(unsigned long sequence, unsigned long status);
void qtFlush();
unsigned long qtDispatch(const unsigned long *values, size_t count);
#endif
