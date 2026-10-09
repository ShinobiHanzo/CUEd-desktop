// Thread-local "last error" string exposed through cued_last_error().
#pragma once

#include <string>

namespace cued {

void setLastError(const std::string& message);
const std::string& lastError();

} // namespace cued
