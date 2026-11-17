import 'package:flutter/material.dart';

import '../build_info.dart';
import '../l10n/l10n.dart';
import '../state/app_scope.dart';
import '../state/app_state.dart';
import '../theme/tokens.dart';
import '../widgets/brand.dart';
import '../widgets/controls.dart';
import '../widgets/group.dart';
import '../widgets/page_scaffold.dart';
import '../widgets/pressable.dart';
import '../widgets/surface.dart';
import 'update_panel.dart';

class AboutPage extends StatelessWidget {
  const AboutPage({super.key});

  @override
  Widget build(BuildContext context) {
    final state = AppScope.of(context);
    final c = SfColors.of(context);
    final l = context.l10n;
    final version = buildVersion.isEmpty ? state.version : buildVersion;
    final needsRestart = buildVersion.isNotEmpty && state.version.isNotEmpty && buildVersion != state.version;
    return PageScaffold(
      title: context.l10n.aboutTitle,
      leading: Pressable(
        onTap: () => state.go(SfPage.settings),
        color: c.card,
        hoverColor: c.card2,
        radius: 22,
        tooltip: context.l10n.aboutBack,
        semanticLabel: context.l10n.aboutBack,
        child: const SizedBox.square(dimension: 44, child: Icon(Icons.arrow_back_ios_new_rounded, size: 18)),
      ),
      children: [
        SfCard(
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              Row(
                children: [
                  const AppIconMark(size: 60),
                  const SizedBox(width: 18),
                  Expanded(
                    child: Column(
                      crossAxisAlignment: CrossAxisAlignment.start,
                      children: [
                        const Text(
                          'FerryDesk',
                          style: TextStyle(fontSize: 26, fontWeight: FontWeight.w800, letterSpacing: -0.5),
                        ),
                        if (personalBuildLabel.isNotEmpty) ...[
                          const SizedBox(height: 6),
                          Text(
                            personalBuildLabel,
                            style: TextStyle(fontSize: 13, color: c.accent, fontWeight: FontWeight.w600),
                          ),
                        ],
                        const SizedBox(height: 6),
                        Text(
                          version.isEmpty ? l.aboutTagline : l.aboutVersionLine(version),
                          style: TextStyle(color: c.muted, height: 1.5),
                        ),
                      ],
                    ),
                  ),
                ],
              ),
              const SizedBox(height: 18),
              if (needsRestart)
                SfRow(
                  icon: Icons.restart_alt_rounded,
                  title: l.aboutRestartBackend,
                  subtitle: l.aboutBackendVersion(state.version),
                )
              else
                const UpdatePanel(),
            ],
          ),
        ),
        SfGroup(
          children: [
            SfRow(
              icon: Icons.update_rounded,
              title: context.l10n.updateAuto,
              subtitle: switch (state.updateInfo.checkedAt) {
                final at? =>
                  '${context.l10n.updateAutoHint} · ${context.l10n.updateLastChecked(timeAgo(context.l10n, at))}',
                null => context.l10n.updateAutoHint,
              },
              trailing: SfSwitch(
                value: state.settings.autoUpdate,
                label: context.l10n.updateAuto,
                onChanged: state.online && !needsRestart
                    ? (v) => state.updateSettings(SettingsPatch(autoUpdate: v))
                    : null,
              ),
            ),
            SfRow(
              icon: Icons.code_rounded,
              title: l.aboutProject,
              subtitle: 'github.com/ccdalao/FerryDesk',
              trailing: Icon(Icons.open_in_new_rounded, size: 18, color: c.muted),
              onTap: () => state.openLink(projectUrl),
            ),
            SfRow(
              icon: Icons.person_outline_rounded,
              title: context.l10n.aboutAuthor,
              subtitle: '小小小龙 · ccdalao',
              trailing: Icon(Icons.open_in_new_rounded, size: 18, color: c.muted),
              onTap: () => state.openLink(maintainerUrl),
            ),
            SfRow(
              icon: Icons.description_outlined,
              title: context.l10n.aboutLicense,
              subtitle: l.aboutLicenseDetail,
              trailing: Icon(Icons.open_in_new_rounded, size: 18, color: c.muted),
              onTap: () => state.openLink('$projectUrl/blob/main/LICENSE-GPL'),
            ),
            SfRow(
              icon: Icons.favorite_border_rounded,
              title: l.aboutCredits,
              subtitle: 'ScreenFerry · Apollo · Moonlight · Virtual Display Driver',
              trailing: Icon(Icons.open_in_new_rounded, size: 18, color: c.muted),
              onTap: () => state.openLink('$projectUrl/blob/main/docs/credits.md'),
            ),
          ],
        ),
      ],
    );
  }
}
