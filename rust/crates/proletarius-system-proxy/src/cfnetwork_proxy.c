#include <CoreFoundation/CoreFoundation.h>
#include <CFNetwork/CFNetwork.h>
#include <CFNetwork/CFProxySupport.h>
#include <stdio.h>
#include <string.h>

typedef struct {
	CFArrayRef proxies;
	int finished;
} SwitchPACResult;

static void switch_pac_callback(void *client, CFArrayRef proxies, CFErrorRef error) {
	SwitchPACResult *result = (SwitchPACResult *)client;
	if (proxies != NULL && error == NULL) result->proxies = CFRetain(proxies);
	result->finished = 1;
}

static int switch_cf_string(CFStringRef value, char *buffer, size_t capacity) {
	if (value == NULL || capacity == 0) return 0;
	if (!CFStringGetCString(value, buffer, (CFIndex)capacity, kCFStringEncodingUTF8)) return 0;
	for (size_t i = 0; buffer[i] != '\0'; i++) {
		if (buffer[i] == '\n' || buffer[i] == '\r') return 0;
	}
	return 1;
}

static CFArrayRef switch_run_pac(CFURLRef target, CFURLRef pac_url, CFStringRef script) {
	CFRunLoopSourceRef source = NULL;
	SwitchPACResult result = { NULL, 0 };
	CFStreamClientContext context = { 0, &result, NULL, NULL, NULL };
	if (pac_url != NULL) {
		source = CFNetworkExecuteProxyAutoConfigurationURL(pac_url, target, switch_pac_callback, &context);
	} else if (script != NULL) {
		source = CFNetworkExecuteProxyAutoConfigurationScript(script, target, switch_pac_callback, &context);
	}
	if (source == NULL) return NULL;
	CFRunLoopAddSource(CFRunLoopGetCurrent(), source, kCFRunLoopDefaultMode);
	CFAbsoluteTime deadline = CFAbsoluteTimeGetCurrent() + 15.0;
	while (!result.finished && CFAbsoluteTimeGetCurrent() < deadline) {
		CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.1, true);
	}
	CFRunLoopSourceInvalidate(source);
	CFRunLoopRemoveSource(CFRunLoopGetCurrent(), source, kCFRunLoopDefaultMode);
	CFRelease(source);
	return result.proxies;
}

static int switch_select_proxy(CFArrayRef proxies, CFURLRef target, char *output, size_t output_size, int depth) {
	if (proxies == NULL || depth > 3) return -1;
	CFIndex count = CFArrayGetCount(proxies);
	for (CFIndex i = 0; i < count; i++) {
		CFDictionaryRef proxy = (CFDictionaryRef)CFArrayGetValueAtIndex(proxies, i);
		CFStringRef type = (CFStringRef)CFDictionaryGetValue(proxy, kCFProxyTypeKey);
		if (type == NULL) continue;
		if (CFEqual(type, kCFProxyTypeNone)) {
			int written = snprintf(output, output_size, "DIRECT");
			return written > 0 && (size_t)written < output_size ? 1 : -1;
		}
		if (CFEqual(type, kCFProxyTypeAutoConfigurationURL) || CFEqual(type, kCFProxyTypeAutoConfigurationJavaScript)) {
			CFURLRef pac_url = CFEqual(type, kCFProxyTypeAutoConfigurationURL)
				? (CFURLRef)CFDictionaryGetValue(proxy, kCFProxyAutoConfigurationURLKey) : NULL;
			CFStringRef script = CFEqual(type, kCFProxyTypeAutoConfigurationJavaScript)
				? (CFStringRef)CFDictionaryGetValue(proxy, kCFProxyAutoConfigurationJavaScriptKey) : NULL;
			CFArrayRef resolved = switch_run_pac(target, pac_url, script);
			if (resolved == NULL) return -1;
			int result = switch_select_proxy(resolved, target, output, output_size, depth + 1);
			CFRelease(resolved);
			if (result != 0) return result;
			continue;
		}
		const char *scheme = NULL;
		if (CFEqual(type, kCFProxyTypeHTTP)) scheme = "http";
		// CFNetwork's HTTPS type selects the proxy for an HTTPS target;
		// the macOS HTTPS proxy setting is still a plain HTTP CONNECT endpoint.
		else if (CFEqual(type, kCFProxyTypeHTTPS)) scheme = "http";
		// Keep target DNS resolution at the SOCKS proxy, including PAC results.
		else if (CFEqual(type, kCFProxyTypeSOCKS)) scheme = "socks5h";
		else continue;
		char host[512] = {0}, username[256] = {0}, password[256] = {0};
		CFNumberRef port_value = (CFNumberRef)CFDictionaryGetValue(proxy, kCFProxyPortNumberKey);
		int port = 0;
		if (!switch_cf_string((CFStringRef)CFDictionaryGetValue(proxy, kCFProxyHostNameKey), host, sizeof(host)) ||
			port_value == NULL || !CFNumberGetValue(port_value, kCFNumberIntType, &port) || port < 1 || port > 65535) continue;
		CFStringRef user_value = (CFStringRef)CFDictionaryGetValue(proxy, kCFProxyUsernameKey);
		CFStringRef password_value = (CFStringRef)CFDictionaryGetValue(proxy, kCFProxyPasswordKey);
		if (user_value != NULL && !switch_cf_string(user_value, username, sizeof(username))) return -1;
		if (password_value != NULL && !switch_cf_string(password_value, password, sizeof(password))) return -1;
		int written = snprintf(output, output_size, "%s\n%s\n%d\n%s\n%s", scheme, host, port, username, password);
		return written > 0 && (size_t)written < output_size ? 1 : -1;
	}
	return 0;
}

static int switch_resolve(CFURLRef target, CFDictionaryRef settings, CFStringRef script, char *output, size_t output_size) {
	if (target == NULL || output == NULL || output_size == 0) return -1;
	CFArrayRef proxies = NULL;
	if (script != NULL) {
		proxies = switch_run_pac(target, NULL, script);
	} else if (settings != NULL) {
		proxies = CFNetworkCopyProxiesForURL(target, settings);
	}
	if (proxies == NULL) return -1;
	int result = switch_select_proxy(proxies, target, output, output_size, 0);
	CFRelease(proxies);
	return result;
}

int switch_cfnetwork_system_proxy(const char *target_utf8, char *output, size_t output_size) {
	if (target_utf8 == NULL || output == NULL || output_size == 0) return -1;
	CFStringRef target_text = CFStringCreateWithCString(kCFAllocatorDefault, target_utf8, kCFStringEncodingUTF8);
	if (target_text == NULL) return -1;
	CFURLRef target = CFURLCreateWithString(kCFAllocatorDefault, target_text, NULL);
	CFRelease(target_text);
	if (target == NULL) return -1;
	CFDictionaryRef settings = CFNetworkCopySystemProxySettings();
	if (settings == NULL) {
		CFRelease(target);
		int written = snprintf(output, output_size, "DIRECT");
		return written > 0 && (size_t)written < output_size ? 1 : -1;
	}
	int result = switch_resolve(target, settings, NULL, output, output_size);
	CFRelease(settings);
	CFRelease(target);
	return result;
}

int switch_cfnetwork_script_proxy(const char *target_utf8, const char *script_utf8, char *output, size_t output_size) {
	if (target_utf8 == NULL || script_utf8 == NULL) return -1;
	CFStringRef target_text = CFStringCreateWithCString(kCFAllocatorDefault, target_utf8, kCFStringEncodingUTF8);
	CFStringRef script = CFStringCreateWithCString(kCFAllocatorDefault, script_utf8, kCFStringEncodingUTF8);
	if (target_text == NULL || script == NULL) {
		if (target_text != NULL) CFRelease(target_text);
		if (script != NULL) CFRelease(script);
		return -1;
	}
	CFURLRef target = CFURLCreateWithString(kCFAllocatorDefault, target_text, NULL);
	CFRelease(target_text);
	if (target == NULL) {
		CFRelease(script);
		return -1;
	}
	int result = switch_resolve(target, NULL, script, output, output_size);
	CFRelease(script);
	CFRelease(target);
	return result;
}

// Test fixture: evaluate explicit macOS proxy settings without changing the
// machine's proxy configuration or contacting the target. kind 1 is the HTTPS
// proxy setting; kind 2 is the SOCKS proxy setting.
int switch_cfnetwork_fixture_proxy(const char *target_utf8, const char *host_utf8, int port, int kind, char *output, size_t output_size) {
	if (target_utf8 == NULL || host_utf8 == NULL || output == NULL || output_size == 0 || port < 1 || port > 65535 || (kind != 1 && kind != 2)) return -1;
	CFStringRef target_text = CFStringCreateWithCString(kCFAllocatorDefault, target_utf8, kCFStringEncodingUTF8);
	CFStringRef host = CFStringCreateWithCString(kCFAllocatorDefault, host_utf8, kCFStringEncodingUTF8);
	if (target_text == NULL || host == NULL) {
		if (target_text != NULL) CFRelease(target_text);
		if (host != NULL) CFRelease(host);
		return -1;
	}
	CFURLRef target = CFURLCreateWithString(kCFAllocatorDefault, target_text, NULL);
	CFRelease(target_text);
	CFNumberRef port_number = CFNumberCreate(kCFAllocatorDefault, kCFNumberIntType, &port);
	int enabled = 1;
	CFNumberRef enabled_number = CFNumberCreate(kCFAllocatorDefault, kCFNumberIntType, &enabled);
	CFMutableDictionaryRef settings = CFDictionaryCreateMutable(kCFAllocatorDefault, 0, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
	int result = -1;
	if (target != NULL && port_number != NULL && enabled_number != NULL && settings != NULL) {
		CFDictionarySetValue(settings, kind == 1 ? kCFNetworkProxiesHTTPSEnable : kCFNetworkProxiesSOCKSEnable, enabled_number);
		CFDictionarySetValue(settings, kind == 1 ? kCFNetworkProxiesHTTPSProxy : kCFNetworkProxiesSOCKSProxy, host);
		CFDictionarySetValue(settings, kind == 1 ? kCFNetworkProxiesHTTPSPort : kCFNetworkProxiesSOCKSPort, port_number);
		result = switch_resolve(target, settings, NULL, output, output_size);
	}
	if (settings != NULL) CFRelease(settings);
	if (enabled_number != NULL) CFRelease(enabled_number);
	if (port_number != NULL) CFRelease(port_number);
	if (target != NULL) CFRelease(target);
	CFRelease(host);
	return result;
}
