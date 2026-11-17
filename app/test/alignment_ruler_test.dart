import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:screenferry_settings/ipc/messages.dart';
import 'package:screenferry_settings/l10n/app_localizations.dart';
import 'package:screenferry_settings/widgets/alignment_ruler.dart';

void main() {
  test('普通设备和扩展屏方案分别保存校准，其他设备设置保留', () {
    const alignment = EdgeAlignment(offset: 200, span: 600);
    const device = DeviceSettings(fingerprint: 'ab', alias: '屏幕', pointerDpi: 2400);
    final changed = device.copyWith(alignment: alignment);
    final decoded = DeviceSettings.fromJson(changed.toJson());
    expect(decoded.alignment, alignment);
    expect(decoded.pointerDpi, 2400);
    expect(decoded.alias, '屏幕');
    const plan = DisplayPlan(
      id: 1,
      name: '工作',
      peer: 'ab',
      localSource: true,
      alignment: EdgeAlignment(offset: -100, span: 1400),
    );
    expect(DisplayPlan.fromJson(plan.toJson()).alignment, plan.alignment);
    expect(plan.alignment, isNot(alignment));
    expect(device.toJson().containsKey('alignment'), false);
  });

  testWidgets('实屏尺完成后显示取点结果，可恢复整边对应', (tester) async {
    var value = const EdgeAlignment();
    final result = Completer<EdgeAlignment?>();
    await tester.pumpWidget(
      MaterialApp(
        locale: const Locale('zh'),
        localizationsDelegates: AppLocalizations.localizationsDelegates,
        supportedLocales: AppLocalizations.supportedLocales,
        home: Scaffold(
          body: StatefulBuilder(
            builder: (context, update) => AlignmentRuler(
              value: value,
              localName: '主控',
              remoteName: '被控',
              side: ScreenSide.right,
              onPick: () => result.future,
              onChanged: (v) => update(() => value = v),
            ),
          ),
        ),
      ),
    );
    await tester.tap(find.byKey(const ValueKey('alignment-pick')));
    await tester.pump();
    expect(find.textContaining('各点两处'), findsOneWidget);
    expect(find.byType(Slider), findsNothing);
    const selected = EdgeAlignment(offset: 200, span: 600, contact: ((200, 800), (0, 1000)));
    result.complete(selected);
    await tester.pumpAndSettle();
    expect(value, selected);
    expect(EdgeAlignment.fromJson(value.toJson()), selected);
    expect(find.textContaining('已取点'), findsOneWidget);
    await tester.tap(find.text('恢复整边对应'));
    await tester.pump();
    expect(value.isDefault, true);
  });
}
