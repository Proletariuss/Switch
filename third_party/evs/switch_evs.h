#ifndef SWITCH_EVS_H
#define SWITCH_EVS_H
#include <stdint.h>
void *switch_evs_create(int amr_wb_mode);
void switch_evs_destroy(void *state);
/* Frame-boundary AMR-WB mode update: 1 on success, 0 without mutation on error. */
int switch_evs_set_amr_wb_mode(void *state, int mode);
int switch_evs_encode(void *state, const int16_t *pcm, uint8_t *bits);
int switch_evs_decode(void *state, const uint8_t *bits, int bit_count, int mode, int lost, int16_t *pcm);
#endif
