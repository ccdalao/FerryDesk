// ScreenFerry scope-exit adapter. SPDX-License-Identifier: GPL-3.0-only
#pragma once
#include <utility>
namespace util {
  template<class F> class fail_guard {
    F function;
    bool active = true;
  public:
    explicit fail_guard(F function): function(std::move(function)) {}
    fail_guard(const fail_guard &) = delete;
    ~fail_guard() noexcept { if (active) function(); }
    void disable() noexcept { active = false; }
  };
}
