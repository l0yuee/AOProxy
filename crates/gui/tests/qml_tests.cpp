#include <QtWidgets/QApplication>
#include <QtQuickTest/quicktest.h>
#include <QtQml/QQmlContext>
#include <QtQml/QQmlEngine>

class NativeMenuTest : public QObject
{
    Q_OBJECT
public:
    Q_INVOKABLE bool activate(QObject *item)
    {
        return QMetaObject::invokeMethod(item, "activate", Qt::DirectConnection);
    }

public slots:
    void qmlEngineAvailable(QQmlEngine *engine)
    {
        engine->rootContext()->setContextProperty("nativeMenuTest", this);
    }
};

int main(int argc, char **argv)
{
    // Qt.labs.platform uses Widgets menus on platforms without native menus.
    QApplication app(argc, argv);
    NativeMenuTest setup;
    return quick_test_main_with_setup(argc, argv, "aoproxy", nullptr, &setup);
}

#include "qml_tests.moc"
