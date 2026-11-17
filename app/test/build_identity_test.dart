import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:screenferry_settings/app.dart';
import 'package:screenferry_settings/build_info.dart';
import 'package:screenferry_settings/state/app_state.dart';

import 'support.dart';

void main() {
  testWidgets('关于页显示界面构建版本，旧后台不能冒充本分支更新结果', (tester) async {
    tester.view.physicalSize = const Size(1160, 900);
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.reset);
    useLocale(tester);
    final state = AppState(backend: DemoBackend(animate: false));
    addTearDown(state.dispose);
    await tester.pumpWidget(ScreenFerryApp(state: state));
    state.start();
    state.go(SfPage.about);
    for (var i = 0; i < 8; i++) {
      await tester.pump(const Duration(milliseconds: 150));
    }
    expect(find.textContaining('版本 $buildVersion'), findsOneWidget);
    expect(find.text('请完成升级并重启后台'), findsOneWidget);
    expect(find.text('已是最新版本'), findsNothing);
    expect(tester.takeException(), isNull);
  }, skip: buildVersion.isEmpty);
}
