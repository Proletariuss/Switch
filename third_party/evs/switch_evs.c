#include <stdlib.h>
#include <string.h>
#include "switch_evs.h"
#include "lib_com/prot.h"
#include "lib_com/cnst.h"
#include "lib_com/stat_com.h"

/* One independent reference encoder/decoder per call. No process-global audio
 * state, subprocess, files, or network activity is used by this adapter. */
typedef struct {
    Encoder_State enc;
    Decoder_State dec;
    Indice indices[MAX_NUM_INDICES];
} SwitchEVS;

static const int amr_wb_rates[9] = {6600,8850,12650,14250,15850,18250,19850,23050,23850};

void *switch_evs_create(int amr_wb_mode) {
    SwitchEVS *s = calloc(1, sizeof(*s));
    if (!s) return NULL;
    s->enc.ind_list = s->indices;
    s->enc.input_Fs = 16000;
    s->enc.Opt_AMR_WB = amr_wb_mode >= 0;
    s->enc.total_brate = amr_wb_mode >= 0 && amr_wb_mode <= 8 ? amr_wb_rates[amr_wb_mode] : 13200;
    s->enc.max_bwidth = WB;
    s->enc.codec_mode = MODE1;
    s->enc.last_codec_mode = MODE1;
    s->enc.Opt_DTX_ON = 0;
    s->enc.interval_SID = FIXED_SID_RATE;
    s->enc.var_SID_rate_flag = 1;
    s->enc.rf_fec_indicator = 1;
    if (init_encoder(&s->enc) != 0) goto failed;
    s->dec.output_Fs = 16000;
    s->dec.bitstreamformat = VOIP_RTPDUMP;
    if (init_decoder(&s->dec) != 0) goto failed;
    reset_indices_dec(&s->dec);
    return s;
failed:
    /* The outer state is zero-initialized, and each native initializer only
     * publishes owned handles that can be destroyed after partial setup. */
    destroy_encoder(&s->enc);
    destroy_decoder(&s->dec);
    free(s);
    return NULL;
}
void switch_evs_destroy(void *state) {
    SwitchEVS *s = state;
    if (!s) return;
    destroy_encoder(&s->enc);
    destroy_decoder(&s->dec);
    free(s);
}
int switch_evs_set_amr_wb_mode(void *state, int mode) {
    SwitchEVS *s = state;
    if (!s || !s->enc.Opt_AMR_WB || mode < 0 || mode > 8) return 0;
    /* Mirror read_next_brate() before the next amr_wb_enc() frame. Preserve
     * all predictor/decoder and last_* state: amr_wb_enc() uses the previous
     * core bitrate to perform its 23.85 kb/s bandwidth-extension transition. */
    s->enc.total_brate = amr_wb_rates[mode];
    s->enc.Opt_SC_VBR = 0;
    s->enc.codec_mode = MODE1;
    return 1;
}
int switch_evs_encode(void *state, const int16_t *pcm, uint8_t *bits) {
    SwitchEVS *s = state;
    if (s->enc.Opt_AMR_WB) amr_wb_enc(&s->enc, pcm, 320);
    else evs_enc(&s->enc, pcm, 320);
    short count = s->enc.nb_bits_tot;
    if (count < 1 || count > 480) { reset_indices_enc(&s->enc); return -1; }
    indices_to_serial(&s->enc, bits, &count);
    reset_indices_enc(&s->enc);
    return count;
}
int switch_evs_decode(void *state, const uint8_t *bits, int bit_count, int mode, int lost, int16_t *pcm) {
    SwitchEVS *s = state;
    unsigned char padded[320 + 2] = {0};
    float output[960] = {0};
    if (bit_count < 0 || bit_count > 2560) return -1;
    if (bits && bit_count) memcpy(padded, bits, (bit_count + 7) / 8);
    read_indices_from_djb(&s->dec, padded, lost ? 0 : bit_count,
                          s->enc.Opt_AMR_WB, (Word16)mode, lost ? 0 : 1, 0, 0);
    if (s->dec.Opt_AMR_WB) amr_wb_dec(&s->dec, output);
    else evs_dec(&s->dec, output,
            s->dec.codec_mode == MODE2 && s->dec.bfi ? FRAMEMODE_MISSING : FRAMEMODE_NORMAL);
    syn_output(output, 320, pcm);
    if (s->dec.ini_frame < MAX_FRAME_COUNTER) s->dec.ini_frame++;
    return 320;
}
